//! BFS crawl + link resolution + style-fix byte rewriter.
//!
//! Mirrors `Sources/Lib/Crawl.swift`. Visited tracking is split by repo scope, both keyed
//! by canonical absolute path. Cross-repo discovery is parallelized via `std::thread::scope`.

use crate::cache::ExtractionCache;
use crate::config::load_project_ignore;
use crate::discovery::{index_repo, RepoIndex};
use crate::extract::{Link, LinkKind};
use crate::path::{base_name, dir_name, real_path, rel_path};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleScope {
    Wiki,
    CrossRepo { repo: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueKind {
    Broken,
    Ambiguous(usize),
    BrokenAnchor(String),
    Style {
        scope: StyleScope,
        suggested: String,
        path_start: usize,
        path_end: usize,
    },
    UnknownRepo(String),
    CrossRepoBroken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkIssue {
    pub link: String,
    pub source: String,
    pub kind: IssueKind,
}

#[derive(Debug, Clone, Default)]
pub struct CrawlOptions {
    /// repo-name → absolute root (env-expanded; will be realpath'd internally).
    pub repos: HashMap<String, String>,
    pub use_default_excludes: bool,
    /// CLI --exclude entries; layered on top of <repo>/.md-orphan and built-in defaults.
    pub extra_excludes: Vec<String>,
}

impl CrawlOptions {
    pub fn new() -> Self {
        Self {
            repos: HashMap::new(),
            use_default_excludes: true,
            extra_excludes: Vec::new(),
        }
    }
}

// MARK: - Resolve helper

/// Resolve a link relative to its source file. Returns absolute path or None if escapes root.
pub fn resolve_link(link: &str, source_file: &str, root: &str) -> Option<String> {
    let source_dir = dir_name(source_file);
    let combined = if link.starts_with('/') {
        format!("{}{}", root, link)
    } else {
        format!("{}/{}", source_dir, link)
    };
    let mut segments: Vec<&str> = Vec::new();
    for seg in combined.split('/').filter(|s| !s.is_empty()) {
        match seg {
            "." => continue,
            ".." => {
                segments.pop();
            }
            _ => segments.push(seg),
        }
    }
    let resolved = format!("/{}", segments.join("/"));
    if resolved == root || resolved.starts_with(&format!("{}/", root)) {
        Some(resolved)
    } else {
        None
    }
}

// MARK: - bfs_crawl

#[derive(Debug, Clone)]
struct BfsQueueItem {
    path: String,
    repo_root: PathBuf,
    is_entry_repo: bool,
}

pub fn bfs_crawl(
    entry_paths: &[String],
    index: RepoIndex,
    options: &CrawlOptions,
    cache: &mut ExtractionCache,
) -> (HashSet<String>, Vec<LinkIssue>) {
    let mut state = CrawlState::new(index, options.clone(), cache);
    state.seed(entry_paths);
    while let Some(item) = state.dequeue() {
        state.visit(&item);
    }
    state.prune_cache();
    (state.reachable, state.issues)
}

/// Convenience: walk + crawl in one call. Returns the index alongside results.
pub fn bfs_crawl_at_root(
    entry_paths: &[String],
    root: &str,
    options: &CrawlOptions,
    cache: &mut ExtractionCache,
) -> (RepoIndex, HashSet<String>, Vec<LinkIssue>) {
    let project_ignore = load_project_ignore(Path::new(root)).unwrap_or_default();
    let mut exclude = options.extra_excludes.clone();
    exclude.extend(project_ignore);
    let idx = index_repo(root, &exclude, options.use_default_excludes, false);
    let (reachable, issues) = bfs_crawl(entry_paths, idx.clone(), options, cache);
    (idx, reachable, issues)
}

// MARK: - CrawlState

struct CrawlState<'c> {
    entry_root: PathBuf,
    entry_index: RepoIndex,
    resolved_repos: HashMap<String, PathBuf>,
    options: CrawlOptions,
    cache: &'c mut ExtractionCache,

    indices: HashMap<PathBuf, RepoIndex>,
    queue: Vec<BfsQueueItem>,
    cursor: usize,
    queued_entry_paths: HashSet<String>,
    cross_repo_visited: HashSet<String>,
    reachable: HashSet<String>,
    issues: Vec<LinkIssue>,
    heading_cache: HashMap<String, BTreeSet<String>>,
}

impl<'c> CrawlState<'c> {
    fn new(entry_index: RepoIndex, options: CrawlOptions, cache: &'c mut ExtractionCache) -> Self {
        let entry_root = entry_index.root.clone();
        let mut resolved_repos = HashMap::new();
        for (name, raw) in &options.repos {
            let resolved = real_path(raw).unwrap_or_else(|| PathBuf::from(raw));
            resolved_repos.insert(name.clone(), resolved);
        }
        let mut indices = HashMap::new();
        indices.insert(entry_root.clone(), entry_index.clone());
        Self {
            entry_root,
            entry_index,
            resolved_repos,
            options,
            cache,
            indices,
            queue: Vec::new(),
            cursor: 0,
            queued_entry_paths: HashSet::new(),
            cross_repo_visited: HashSet::new(),
            reachable: HashSet::new(),
            issues: Vec::new(),
            heading_cache: HashMap::new(),
        }
    }

    fn seed(&mut self, entry_paths: &[String]) {
        for ep in entry_paths {
            let canonical = real_path(ep)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ep.clone());
            self.queued_entry_paths.insert(canonical.clone());
            self.queue.push(BfsQueueItem {
                path: canonical,
                repo_root: self.entry_root.clone(),
                is_entry_repo: true,
            });
        }
        self.prefetch_referenced_repos();
    }

    /// Walk seeded entry files once to find first-level cross-repo target names, then
    /// parallel-index those repos via `std::thread::scope`.
    fn prefetch_referenced_repos(&mut self) {
        let mut targets: HashSet<PathBuf> = HashSet::new();
        for item in self.queue.clone() {
            if !item.is_entry_repo {
                continue;
            }
            let Some(result) = self.cache.read(Path::new(&item.path), &item.repo_root) else {
                continue;
            };
            for link in &result.links {
                if let LinkKind::CrossRepo { repo } = &link.kind {
                    if let Some(root) = self.resolved_repos.get(repo) {
                        targets.insert(root.clone());
                    }
                }
            }
        }
        let to_index: Vec<PathBuf> = targets
            .into_iter()
            .filter(|r| r != &self.entry_root)
            .collect();
        if to_index.is_empty() {
            return;
        }
        let prefetched = Mutex::new(HashMap::<PathBuf, RepoIndex>::new());
        let extra_excludes = self.options.extra_excludes.clone();
        let use_defaults = self.options.use_default_excludes;
        std::thread::scope(|s| {
            for root in &to_index {
                let prefetched = &prefetched;
                let extra_excludes = extra_excludes.clone();
                s.spawn(move || {
                    let project_ignore = load_project_ignore(root).unwrap_or_default();
                    let mut excl = extra_excludes;
                    excl.extend(project_ignore);
                    let root_str = root.to_string_lossy();
                    let idx = index_repo(&root_str, &excl, use_defaults, false);
                    prefetched.lock().unwrap().insert(root.clone(), idx);
                });
            }
        });
        let prefetched = prefetched.into_inner().unwrap();
        for (root, idx) in prefetched {
            self.indices.insert(root, idx);
        }
    }

    fn dequeue(&mut self) -> Option<BfsQueueItem> {
        if self.cursor >= self.queue.len() {
            return None;
        }
        let item = self.queue[self.cursor].clone();
        self.cursor += 1;
        Some(item)
    }

    fn visit(&mut self, item: &BfsQueueItem) {
        let Some(result) = self.cache.read(Path::new(&item.path), &item.repo_root) else {
            eprintln!("md-orphan: warning: cannot read {}", item.path);
            return;
        };
        if item.is_entry_repo {
            if !self.reachable.insert(item.path.clone()) {
                return;
            }
        } else if !self.cross_repo_visited.insert(item.path.clone()) {
            return;
        }
        self.heading_cache.insert(item.path.clone(), result.headings.clone());
        for link in result.links {
            self.resolve_one(&link, item);
        }
    }

    fn index_for(&mut self, canonical_root: &Path) -> &RepoIndex {
        if !self.indices.contains_key(canonical_root) {
            let project_ignore = load_project_ignore(canonical_root).unwrap_or_default();
            let mut excl = self.options.extra_excludes.clone();
            excl.extend(project_ignore);
            let root_str = canonical_root.to_string_lossy();
            let idx = index_repo(
                &root_str,
                &excl,
                self.options.use_default_excludes,
                false,
            );
            self.indices.insert(canonical_root.to_path_buf(), idx);
        }
        self.indices.get(canonical_root).unwrap()
    }

    fn headings_for(&mut self, canonical: &str) -> BTreeSet<String> {
        if let Some(cached) = self.heading_cache.get(canonical) {
            return cached.clone();
        }
        let owning_root = self
            .repo_root_containing(canonical)
            .unwrap_or_else(|| self.entry_root.clone());
        let h = self
            .cache
            .read(Path::new(canonical), &owning_root)
            .map(|r| r.headings)
            .unwrap_or_default();
        self.heading_cache.insert(canonical.to_string(), h.clone());
        h
    }

    fn repo_root_containing(&self, path: &str) -> Option<PathBuf> {
        let entry_str = self.entry_root.to_string_lossy();
        if path == entry_str.as_ref() || path.starts_with(&format!("{}/", entry_str)) {
            return Some(self.entry_root.clone());
        }
        for root in self.resolved_repos.values() {
            let s = root.to_string_lossy();
            if path == s.as_ref() || path.starts_with(&format!("{}/", s)) {
                return Some(root.clone());
            }
        }
        None
    }

    fn resolve_one(&mut self, link: &Link, source: &BfsQueueItem) {
        match &link.kind {
            LinkKind::Wiki | LinkKind::Standard => self.resolve_same_repo(link, source),
            LinkKind::CrossRepo { repo } => {
                let repo = repo.clone();
                self.resolve_cross_repo(link, source, &repo);
            }
        }
    }

    fn resolve_same_repo(&mut self, link: &Link, source: &BfsQueueItem) {
        let current = self.index_for(&source.repo_root).clone();
        let current_root_str = current.root.to_string_lossy().to_string();
        let Some(resolved) = resolve_link(&link.target, &source.path, &current_root_str) else {
            return;
        };
        let is_md = link.target.ends_with(".md");
        let mut canonical = real_path(&resolved).map(|p| p.to_string_lossy().to_string());

        // Basename fallback for .md links.
        if canonical.is_none() && is_md {
            let basename = base_name(&link.target);
            if let Some(candidates) = current.by_name.get(basename) {
                match candidates.len() {
                    1 => {
                        canonical = real_path(&candidates[0]).map(|p| p.to_string_lossy().to_string());
                    }
                    n if n > 1 => {
                        self.issues.push(LinkIssue {
                            link: link.target.clone(),
                            source: source.path.clone(),
                            kind: IssueKind::Ambiguous(n),
                        });
                        return;
                    }
                    _ => {}
                }
            }
        }
        let Some(canonical) = canonical else {
            self.issues.push(LinkIssue {
                link: link.target.clone(),
                source: source.path.clone(),
                kind: IssueKind::Broken,
            });
            return;
        };

        // Style check: wiki links only, target must live in same repo.
        if matches!(link.kind, LinkKind::Wiki)
            && canonical.starts_with(&format!("{}/", current_root_str))
        {
            self.emit_style_if_needed(
                link,
                &source.path,
                &canonical,
                &current.root,
                &current.by_name,
                StyleScope::Wiki,
            );
        }

        if !is_md {
            return;
        }

        if let Some(fragment) = &link.fragment {
            let h = self.headings_for(&canonical);
            if !h.contains(fragment) {
                self.issues.push(LinkIssue {
                    link: link.target.clone(),
                    source: source.path.clone(),
                    kind: IssueKind::BrokenAnchor(fragment.clone()),
                });
            }
        }

        // Enqueue.
        let owning_root = self
            .repo_root_containing(&canonical)
            .unwrap_or_else(|| current.root.clone());
        self.enqueue_resolved(canonical, owning_root);
    }

    fn resolve_cross_repo(&mut self, link: &Link, source: &BfsQueueItem, repo_name: &str) {
        let Some(repo_root) = self.resolved_repos.get(repo_name).cloned() else {
            self.issues.push(LinkIssue {
                link: link.target.clone(),
                source: source.path.clone(),
                kind: IssueKind::UnknownRepo(repo_name.to_string()),
            });
            return;
        };
        let repo_root_str = repo_root.to_string_lossy().to_string();
        let raw_combined = if link.target.starts_with('/') {
            format!("{}{}", repo_root_str, link.target)
        } else {
            format!("{}/{}", repo_root_str, link.target)
        };
        let mut segments: Vec<&str> = Vec::new();
        let mut escaped = false;
        for seg in raw_combined.split('/').filter(|s| !s.is_empty()) {
            match seg {
                "." => continue,
                ".." => {
                    if segments.is_empty() {
                        escaped = true;
                        break;
                    }
                    segments.pop();
                }
                _ => segments.push(seg),
            }
        }
        let normalized = format!("/{}", segments.join("/"));
        let within_root = !escaped
            && (normalized == repo_root_str
                || normalized.starts_with(&format!("{}/", repo_root_str)));

        let is_md = link.target.ends_with(".md");
        let mut canonical = if within_root {
            real_path(&normalized).map(|p| p.to_string_lossy().to_string())
        } else {
            None
        };
        let repo_index = self.index_for(&repo_root).clone();

        if canonical.is_none() && is_md {
            let basename = base_name(&link.target);
            if let Some(candidates) = repo_index.by_name.get(basename) {
                match candidates.len() {
                    1 => {
                        canonical = real_path(&candidates[0]).map(|p| p.to_string_lossy().to_string());
                    }
                    n if n > 1 => {
                        self.issues.push(LinkIssue {
                            link: link.target.clone(),
                            source: source.path.clone(),
                            kind: IssueKind::Ambiguous(n),
                        });
                        return;
                    }
                    _ => {}
                }
            }
        }
        let Some(canonical) = canonical else {
            self.issues.push(LinkIssue {
                link: link.target.clone(),
                source: source.path.clone(),
                kind: IssueKind::CrossRepoBroken(repo_name.to_string()),
            });
            return;
        };

        if canonical.starts_with(&format!("{}/", repo_root_str)) {
            self.emit_style_if_needed(
                link,
                &source.path,
                &canonical,
                &repo_root,
                &repo_index.by_name,
                StyleScope::CrossRepo {
                    repo: repo_name.to_string(),
                },
            );
        }

        if !is_md {
            return;
        }

        if let Some(fragment) = &link.fragment {
            let h = self.headings_for(&canonical);
            if !h.contains(fragment) {
                self.issues.push(LinkIssue {
                    link: link.target.clone(),
                    source: source.path.clone(),
                    kind: IssueKind::BrokenAnchor(fragment.clone()),
                });
            }
        }

        let entry_str = self.entry_root.to_string_lossy().to_string();
        let owning_root = if canonical == entry_str
            || canonical.starts_with(&format!("{}/", entry_str))
        {
            self.entry_root.clone()
        } else {
            repo_root
        };
        self.enqueue_resolved(canonical, owning_root);
    }

    fn emit_style_if_needed(
        &mut self,
        link: &Link,
        source: &str,
        canonical: &str,
        repo_root: &Path,
        by_name: &HashMap<String, Vec<String>>,
        scope: StyleScope,
    ) {
        let root_str = repo_root.to_string_lossy().to_string();
        let Some(rel_target) = rel_path(canonical, &root_str).map(|s| s.to_string()) else {
            return;
        };
        let basename = base_name(&rel_target);
        let canonical_form = if let Some(cands) = by_name.get(basename) {
            if cands.len() == 1 {
                basename.to_string()
            } else {
                rel_target.clone()
            }
        } else {
            rel_target.clone()
        };
        if link.target == canonical_form {
            return;
        }
        self.issues.push(LinkIssue {
            link: link.target.clone(),
            source: source.to_string(),
            kind: IssueKind::Style {
                scope,
                suggested: canonical_form,
                path_start: link.path_start,
                path_end: link.path_end,
            },
        });
    }

    fn enqueue_resolved(&mut self, canonical: String, owning_root: PathBuf) {
        if owning_root == self.entry_root {
            if self.queued_entry_paths.insert(canonical.clone()) {
                self.queue.push(BfsQueueItem {
                    path: canonical,
                    repo_root: self.entry_root.clone(),
                    is_entry_repo: true,
                });
            }
        } else if !self.cross_repo_visited.contains(&canonical) {
            self.queue.push(BfsQueueItem {
                path: canonical,
                repo_root: owning_root,
                is_entry_repo: false,
            });
        }
    }

    fn prune_cache(&mut self) {
        let snapshot: Vec<(PathBuf, HashSet<String>)> = self
            .indices
            .iter()
            .map(|(r, i)| (r.clone(), i.md_files.clone()))
            .collect();
        for (root, keep) in snapshot {
            self.cache.prune(&root, &keep);
        }
    }
}

// MARK: - --fix byte rewriter

/// Rewrite the path bytes inside `[[...]]` or `` `...` `` for every `.style` issue.
/// Replacements per source file are applied in descending byte-offset order so earlier
/// offsets stay valid. Atomic write via `tempfile`.
pub fn apply_style_fixes(issues: &[LinkIssue]) {
    let mut by_source: HashMap<String, Vec<&LinkIssue>> = HashMap::new();
    for i in issues {
        by_source.entry(i.source.clone()).or_default().push(i);
    }
    for (source, mut source_issues) in by_source {
        let Ok(data) = fs::read(&source) else {
            eprintln!("md-orphan: warning: cannot read {source} for --fix");
            continue;
        };
        // Sort by path_start descending so earlier offsets stay valid.
        source_issues.sort_by(|a, b| {
            let av = match &a.kind {
                IssueKind::Style { path_start, .. } => *path_start,
                _ => 0,
            };
            let bv = match &b.kind {
                IssueKind::Style { path_start, .. } => *path_start,
                _ => 0,
            };
            bv.cmp(&av)
        });
        let mut bytes = data;
        for issue in &source_issues {
            if let IssueKind::Style {
                suggested,
                path_start,
                path_end,
                ..
            } = &issue.kind
            {
                if path_start <= path_end && *path_end <= bytes.len() {
                    bytes.splice(*path_start..*path_end, suggested.as_bytes().iter().copied());
                }
            }
        }
        // Atomic write via tempfile in the same dir.
        let path = Path::new(&source);
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        match tempfile::NamedTempFile::new_in(parent) {
            Ok(mut tmp) => {
                if let Err(e) = tmp.write_all(&bytes) {
                    eprintln!("md-orphan: warning: cannot write {source}: {e}");
                    continue;
                }
                if let Err(e) = tmp.persist(path) {
                    eprintln!("md-orphan: warning: cannot persist {source}: {e}");
                }
            }
            Err(e) => eprintln!("md-orphan: warning: cannot create tmp for {source}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::LinkKind;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn resolves_simple_link() {
        let r = resolve_link("guide.md", "/repo/docs/index.md", "/repo");
        assert_eq!(r.as_deref(), Some("/repo/docs/guide.md"));
    }

    #[test]
    fn resolves_parent_traversal() {
        let r = resolve_link("../dev/guide.md", "/repo/docs/system/langpack.md", "/repo");
        assert_eq!(r.as_deref(), Some("/repo/docs/dev/guide.md"));
    }

    #[test]
    fn resolves_dot_segment() {
        let r = resolve_link("./local.md", "/repo/docs/index.md", "/repo");
        assert_eq!(r.as_deref(), Some("/repo/docs/local.md"));
    }

    #[test]
    fn rejects_root_escape() {
        let r = resolve_link("../../../etc/passwd.md", "/repo/docs/index.md", "/repo");
        assert!(r.is_none());
    }

    #[test]
    fn resolves_absolute_link() {
        let r = resolve_link("/docs/file.md", "/repo/other/index.md", "/repo");
        assert_eq!(r.as_deref(), Some("/repo/docs/file.md"));
    }

    #[test]
    fn finds_broken_links() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[a](missing.md)");
        let mut cache = ExtractionCache::new(false);
        let (_, _, issues) = bfs_crawl_at_root(
            &[canonical.join("index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        let broken: Vec<_> = issues.iter().filter(|i| matches!(i.kind, IssueKind::Broken)).collect();
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].link, "missing.md");
    }

    #[test]
    fn no_broken_on_valid_links() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[a](other.md)");
        write(&canonical.join("other.md"), "hello");
        let mut cache = ExtractionCache::new(false);
        let (_, reachable, issues) = bfs_crawl_at_root(
            &[canonical.join("index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        assert!(issues.is_empty());
        assert_eq!(reachable.len(), 2);
    }

    #[test]
    fn basename_fallback() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[a](guide.md)");
        write(&canonical.join("docs/guide.md"), "hello");
        let mut cache = ExtractionCache::new(false);
        let (_, reachable, issues) = bfs_crawl_at_root(
            &[canonical.join("index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        assert!(issues.is_empty());
        assert_eq!(reachable.len(), 2);
    }

    #[test]
    fn ambiguous_link() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[a](guide.md)");
        write(&canonical.join("a/guide.md"), "");
        write(&canonical.join("b/guide.md"), "");
        let mut cache = ExtractionCache::new(false);
        let (_, _, issues) = bfs_crawl_at_root(
            &[canonical.join("index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        let ambig: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Ambiguous(_)))
            .collect();
        assert_eq!(ambig.len(), 1);
        if let IssueKind::Ambiguous(n) = ambig[0].kind {
            assert_eq!(n, 2);
        }
    }

    #[test]
    fn broken_anchor() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[ref](other.md#missing-section)");
        write(&canonical.join("other.md"), "# Existing Section\n\nSome content");
        let mut cache = ExtractionCache::new(false);
        let (_, _, issues) = bfs_crawl_at_root(
            &[canonical.join("index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        let anchors: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::BrokenAnchor(_)))
            .collect();
        assert_eq!(anchors.len(), 1);
        if let IssueKind::BrokenAnchor(frag) = &anchors[0].kind {
            assert_eq!(frag, "missing-section");
        }
    }

    #[test]
    fn wiki_style_relative_path_flagged() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("docs/dev/index.md"), "see [[../system/foo.md]]");
        write(&canonical.join("docs/system/foo.md"), "");
        let mut cache = ExtractionCache::new(false);
        let (_, _, issues) = bfs_crawl_at_root(
            &[canonical.join("docs/dev/index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        let style: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Style { .. }))
            .collect();
        assert_eq!(style.len(), 1);
        if let IssueKind::Style { suggested, .. } = &style[0].kind {
            assert_eq!(suggested, "foo.md");
        }
    }

    #[test]
    fn cross_repo_unknown_repo_flagged() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "see `foo.md` (no-such-repo)");
        let mut cache = ExtractionCache::new(false);
        let (_, _, issues) = bfs_crawl_at_root(
            &[canonical.join("index.md").to_string_lossy().to_string()],
            canonical.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        let unknowns: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::UnknownRepo(_)))
            .collect();
        assert_eq!(unknowns.len(), 1);
        if let IssueKind::UnknownRepo(r) = &unknowns[0].kind {
            assert_eq!(r, "no-such-repo");
        }
    }

    #[test]
    fn fix_rewrites_wiki_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        let entry = canonical.join("a/index.md");
        write(&entry, "x [[docs/guide.md]] y");
        write(&canonical.join("a/docs/guide.md"), "");
        let mut cache = ExtractionCache::new(false);
        let (_, _, issues) = bfs_crawl_at_root(
            &[entry.to_string_lossy().to_string()],
            canonical.join("a").to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        let style: Vec<&LinkIssue> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Style { .. }))
            .collect();
        assert_eq!(style.len(), 1);

        // Apply fix.
        let style_owned: Vec<LinkIssue> = style.into_iter().cloned().collect();
        apply_style_fixes(&style_owned);

        let rewritten = fs::read_to_string(&entry).unwrap();
        assert_eq!(rewritten, "x [[guide.md]] y");

        // Idempotency: re-run yields no style issues.
        let mut cache2 = ExtractionCache::new(false);
        let (_, _, issues2) = bfs_crawl_at_root(
            &[entry.to_string_lossy().to_string()],
            canonical.join("a").to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache2,
        );
        let style2: Vec<_> = issues2
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Style { .. }))
            .collect();
        assert!(style2.is_empty());
    }
}
