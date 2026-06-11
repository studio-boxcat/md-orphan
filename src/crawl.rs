//! BFS crawl + link resolution + style-fix byte rewriter.
//!
//! Mirrors `Sources/Lib/Crawl.swift`. Visited tracking is split by repo scope, both keyed
//! by canonical absolute path. Cross-repo discovery is parallelized via `std::thread::scope`.

use crate::cache::ExtractionCache;
use crate::config::load_project_ignore;
use crate::discovery::{index_repo, RepoIndex};
use crate::extract::{anchor_id, Link, LinkKind};
use crate::path::{
    atomic_write_bytes, base_name, canonicalize_str, dir_name, is_under, rel_path,
    CanonicalPath,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::rc::Rc;
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
    CrossRepoBroken(String),
    /// A reachable file that couldn't be read (vanished mid-run, permissions). The crawl can't
    /// verify its links/headings, so the run must not exit clean. `link` == `source` == the file.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkIssue {
    pub link: String,
    pub source: String,
    pub kind: IssueKind,
}

// No `Default` derive: it would yield `use_default_excludes: false, use_walk_cache: false`,
// silently contradicting `new()`'s documented defaults. All construction goes through `new()`.
#[derive(Debug, Clone)]
pub struct CrawlOptions {
    /// repo-name → absolute root (env-expanded; will be realpath'd internally).
    pub repos: HashMap<String, String>,
    pub use_default_excludes: bool,
    /// CLI --exclude entries; layered on top of <repo>/.md-orphan and built-in defaults.
    pub extra_excludes: Vec<String>,
    /// See [[walk_cache.rs]]. Toggled with the per-file cache by `--no-cache`.
    pub use_walk_cache: bool,
    /// Index every extension into `by_name` so non-`.md` wiki refs get style and
    /// basename resolution. ~30× walk cost on Unity-scale repos — CLI `--all-extensions`.
    pub include_all_extensions: bool,
}

impl CrawlOptions {
    // See the struct comment — a Default impl is exactly the footgun being avoided.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            repos: HashMap::new(),
            use_default_excludes: true,
            extra_excludes: Vec::new(),
            use_walk_cache: true,
            include_all_extensions: false,
        }
    }
}

// MARK: - Resolve helper

/// Fold `.`/`..` segments of an absolute path into a normalized `/`-rooted string.
///
/// `strict_root` governs a `..` encountered with no segments left to pop (i.e. trying to climb
/// above filesystem root):
/// - `false` (same-repo): silent no-op. Containment is judged only by the caller's final prefix
///   check, so a path that climbs out and re-enters (`/a/b/../../../a/b/c.md`) still resolves.
/// - `true` (cross-repo): abort with `escaped = true`. Any path that transiently climbs above the
///   configured repo root is rejected outright, even if it would re-enter.
///
/// Returns the normalized path and whether a strict escape was hit (always `false` when lenient).
fn fold_segments(combined: &str, strict_root: bool) -> (String, bool) {
    let mut segments: Vec<&str> = Vec::new();
    let mut escaped = false;
    for seg in combined.split('/').filter(|s| !s.is_empty()) {
        match seg {
            "." => continue,
            ".." => {
                if segments.is_empty() {
                    if strict_root {
                        escaped = true;
                        break;
                    }
                } else {
                    segments.pop();
                }
            }
            _ => segments.push(seg),
        }
    }
    (format!("/{}", segments.join("/")), escaped)
}

/// Resolve a link relative to its source file. Returns absolute path or None if escapes root.
pub fn resolve_link(link: &str, source_file: &str, root: &str) -> Option<String> {
    let source_dir = dir_name(source_file);
    let combined = if link.starts_with('/') {
        format!("{}{}", root, link)
    } else {
        format!("{}/{}", source_dir, link)
    };
    let (resolved, _) = fold_segments(&combined, false);
    if is_under(&resolved, root) {
        Some(resolved)
    } else {
        None
    }
}

/// Root-relative strict resolve for cross-repo refs: join under `root`, fold with the
/// strict escape rule, require containment, then realpath. `None` = no such file, or the
/// path escaped the root.
fn resolve_under_root(target: &str, root: &str) -> Option<CanonicalPath> {
    let raw_combined = if target.starts_with('/') {
        format!("{}{}", root, target)
    } else {
        format!("{}/{}", root, target)
    };
    let (normalized, escaped) = fold_segments(&raw_combined, true);
    if escaped || !is_under(&normalized, root) {
        return None;
    }
    canonicalize_str(&normalized)
}

/// `.md` basename fallback: when a path-resolve misses, look the link's basename up in the repo's
/// name map. `Ok(Some)` = unique match resolved (or `None` if its realpath fails), `Ok(None)` = no
/// such basename, `Err(n)` = n>1 candidates (ambiguous). Shared by same-repo and cross-repo resolve.
/// `by_name` values may end in a symlinked *file* (the walk doesn't resolve those), hence the
/// re-canonicalize here — this is where the `CanonicalPath` brand gets applied.
fn basename_fallback(
    target: &str,
    by_name: &HashMap<String, Vec<String>>,
) -> Result<Option<CanonicalPath>, usize> {
    let Some(candidates) = by_name.get(base_name(target)) else {
        return Ok(None);
    };
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(canonicalize_str(&candidates[0])),
        n => Err(n),
    }
}

// MARK: - bfs_crawl

#[derive(Debug, Clone)]
struct BfsQueueItem {
    path: CanonicalPath,
    repo_root: CanonicalPath,
    is_entry_repo: bool,
}

pub fn bfs_crawl(
    entry_paths: &[String],
    index: RepoIndex,
    options: &CrawlOptions,
    cache: &mut ExtractionCache,
) -> (HashSet<CanonicalPath>, Vec<LinkIssue>) {
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
) -> (RepoIndex, HashSet<CanonicalPath>, Vec<LinkIssue>) {
    let project_ignore = load_project_ignore(Path::new(root)).unwrap_or_default();
    let mut exclude = options.extra_excludes.clone();
    exclude.extend(project_ignore);
    let idx = index_repo(
        root,
        &exclude,
        options.use_default_excludes,
        options.include_all_extensions,
        options.use_walk_cache,
    );
    let (reachable, issues) = bfs_crawl(entry_paths, idx.clone(), options, cache);
    (idx, reachable, issues)
}

// MARK: - CrawlState

struct CrawlState<'c> {
    entry_root: CanonicalPath,
    resolved_repos: HashMap<String, CanonicalPath>,
    options: CrawlOptions,
    cache: &'c mut ExtractionCache,

    /// `Rc` so resolvers can hold the index across `&mut self` issue-pushes without deep-cloning
    /// it per link — `by_name`/`md_files` are large (15k+ entries on Unity-scale repos).
    indices: HashMap<CanonicalPath, Rc<RepoIndex>>,
    queue: Vec<BfsQueueItem>,
    cursor: usize,
    queued_entry_paths: HashSet<CanonicalPath>,
    cross_repo_visited: HashSet<CanonicalPath>,
    reachable: HashSet<CanonicalPath>,
    issues: Vec<LinkIssue>,
    heading_cache: HashMap<CanonicalPath, BTreeSet<String>>,
}

impl<'c> CrawlState<'c> {
    fn new(entry_index: RepoIndex, options: CrawlOptions, cache: &'c mut ExtractionCache) -> Self {
        let entry_root = entry_index.root.clone();
        let mut resolved_repos = HashMap::new();
        for (name, raw) in &options.repos {
            // retain_existing() already dropped non-dirs, so realpath only fails on races.
            let resolved = canonicalize_str(raw)
                .unwrap_or_else(|| CanonicalPath::assume_canonical(raw.clone()));
            resolved_repos.insert(name.clone(), resolved);
        }
        let mut indices = HashMap::new();
        indices.insert(entry_root.clone(), Rc::new(entry_index));
        Self {
            entry_root,
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
            // Missing entry has no realpath; brand as-is so visit() reports it Unreadable.
            let canonical = canonicalize_str(ep)
                .unwrap_or_else(|| CanonicalPath::assume_canonical(ep.clone()));
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
        let mut targets: HashSet<CanonicalPath> = HashSet::new();
        // Field-level split borrows: &self.queue (read) + &mut self.cache (read-through) are
        // disjoint, so no queue clone is needed.
        for item in &self.queue {
            if !item.is_entry_repo {
                continue;
            }
            let Some(result) = self.cache.read(item.path.as_ref(), item.repo_root.as_ref()) else {
                continue;
            };
            for link in &result.links {
                if let LinkKind::CrossRepo { repo } = &link.kind
                    && let Some(root) = self.resolved_repos.get(repo) {
                        targets.insert(root.clone());
                    }
            }
        }
        let to_index: Vec<CanonicalPath> = targets
            .into_iter()
            .filter(|r| r != &self.entry_root)
            .collect();
        if to_index.is_empty() {
            return;
        }
        let prefetched = Mutex::new(HashMap::<CanonicalPath, RepoIndex>::new());
        let extra_excludes = self.options.extra_excludes.clone();
        let use_defaults = self.options.use_default_excludes;
        let use_walk_cache = self.options.use_walk_cache;
        let all_ext = self.options.include_all_extensions;
        std::thread::scope(|s| {
            for root in &to_index {
                let prefetched = &prefetched;
                let extra_excludes = extra_excludes.clone();
                s.spawn(move || {
                    let project_ignore = load_project_ignore(root.as_ref()).unwrap_or_default();
                    let mut excl = extra_excludes;
                    excl.extend(project_ignore);
                    let idx = index_repo(root.as_str(), &excl, use_defaults, all_ext, use_walk_cache);
                    prefetched.lock().unwrap().insert(root.clone(), idx);
                });
            }
        });
        let prefetched = prefetched.into_inner().unwrap();
        for (root, idx) in prefetched {
            self.indices.insert(root, Rc::new(idx));
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
        // Mark visited before reading: an unreadable file is still *reachable* (a link led
        // here), so it must not double-report as an orphan — and re-reading a failing path
        // on every re-encounter is useless.
        if item.is_entry_repo {
            if !self.reachable.insert(item.path.clone()) {
                return;
            }
        } else if !self.cross_repo_visited.insert(item.path.clone()) {
            return;
        }
        let Some(result) = self.cache.read(item.path.as_ref(), item.repo_root.as_ref()) else {
            self.issues.push(LinkIssue {
                link: item.path.to_string(),
                source: item.path.to_string(),
                kind: IssueKind::Unreadable,
            });
            return;
        };
        self.heading_cache.insert(item.path.clone(), result.headings.clone());
        // Scope: don't follow links inside cross-repo files. Those repos run their own
        // md-orphan; their broken-link rot isn't this entry repo's responsibility. Headings
        // are still cached above so anchor checks on entry-repo's direct refs into this file
        // continue to work.
        if !item.is_entry_repo {
            return;
        }
        for link in result.links {
            self.resolve_one(&link, item);
        }
    }

    /// Returned `Rc` is a pointer bump — callers hold it across `&mut self` calls for free.
    fn index_for(&mut self, canonical_root: &CanonicalPath) -> Rc<RepoIndex> {
        if !self.indices.contains_key(canonical_root) {
            let project_ignore = load_project_ignore(canonical_root.as_ref()).unwrap_or_default();
            let mut excl = self.options.extra_excludes.clone();
            excl.extend(project_ignore);
            let idx = index_repo(
                canonical_root.as_str(),
                &excl,
                self.options.use_default_excludes,
                self.options.include_all_extensions,
                self.options.use_walk_cache,
            );
            self.indices.insert(canonical_root.clone(), Rc::new(idx));
        }
        Rc::clone(self.indices.get(canonical_root).unwrap())
    }

    fn headings_for(&mut self, canonical: &CanonicalPath) -> BTreeSet<String> {
        if let Some(cached) = self.heading_cache.get(canonical) {
            return cached.clone();
        }
        let owning_root = self
            .repo_root_containing(canonical.as_str())
            .unwrap_or_else(|| self.entry_root.clone());
        let h = self
            .cache
            .read(canonical.as_ref(), owning_root.as_ref())
            .map(|r| r.headings)
            .unwrap_or_default();
        self.heading_cache.insert(canonical.clone(), h.clone());
        h
    }

    fn repo_root_containing(&self, path: &str) -> Option<CanonicalPath> {
        if is_under(path, self.entry_root.as_str()) {
            return Some(self.entry_root.clone());
        }
        for root in self.resolved_repos.values() {
            if is_under(path, root.as_str()) {
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
        // Self-anchor link `[[#section]]`: empty target means "this same file". Validate the
        // fragment against the source's own headings; no path to resolve, style-check, or enqueue.
        if link.target.is_empty() {
            if let Some(fragment) = &link.fragment {
                self.validate_fragment(link, &source.path, &source.path, fragment);
            }
            return;
        }
        let current = self.index_for(&source.repo_root);
        let Some(resolved) = resolve_link(&link.target, source.path.as_str(), current.root.as_str())
        else {
            return;
        };
        let is_md = link.target.ends_with(".md");
        let mut canonical = canonicalize_str(&resolved);

        // Basename fallback for .md links — and for any extension when by_name indexes all.
        if canonical.is_none() && (is_md || self.options.include_all_extensions) {
            match basename_fallback(&link.target, &current.by_name) {
                Ok(resolved) => canonical = resolved,
                Err(n) => {
                    self.issues.push(LinkIssue {
                        link: link.target.clone(),
                        source: source.path.to_string(),
                        kind: IssueKind::Ambiguous(n),
                    });
                    return;
                }
            }
        }
        let Some(canonical) = canonical else {
            self.issues.push(LinkIssue {
                link: link.target.clone(),
                source: source.path.to_string(),
                kind: IssueKind::Broken,
            });
            return;
        };

        // Style check: wiki links only, target must live in same repo.
        if matches!(link.kind, LinkKind::Wiki) && is_under(canonical.as_str(), current.root.as_str()) {
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
            self.validate_fragment(link, &source.path, &canonical, fragment);
        }

        // Enqueue.
        let owning_root = self
            .repo_root_containing(canonical.as_str())
            .unwrap_or_else(|| current.root.clone());
        self.enqueue_resolved(canonical, owning_root);
    }

    fn resolve_cross_repo(&mut self, link: &Link, source: &BfsQueueItem, repo_name: &str) {
        // Parser pre-filters unknown names; this branch is unreachable from the CLI path.
        let Some(repo_root) = self.resolved_repos.get(repo_name).cloned() else {
            return;
        };
        let is_md = link.target.ends_with(".md");
        let mut canonical = resolve_under_root(&link.target, repo_root.as_str());
        let repo_index = self.index_for(&repo_root);

        if canonical.is_none() && is_md {
            match basename_fallback(&link.target, &repo_index.by_name) {
                Ok(resolved) => canonical = resolved,
                Err(n) => {
                    self.issues.push(LinkIssue {
                        link: link.target.clone(),
                        source: source.path.to_string(),
                        kind: IssueKind::Ambiguous(n),
                    });
                    return;
                }
            }
        }
        let Some(canonical) = canonical else {
            self.issues.push(LinkIssue {
                link: link.target.clone(),
                source: source.path.to_string(),
                kind: IssueKind::CrossRepoBroken(repo_name.to_string()),
            });
            return;
        };

        if is_under(canonical.as_str(), repo_root.as_str()) {
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
            self.validate_fragment(link, &source.path, &canonical, fragment);
        }

        let owning_root = if is_under(canonical.as_str(), self.entry_root.as_str()) {
            self.entry_root.clone()
        } else {
            repo_root
        };
        self.enqueue_resolved(canonical, owning_root);
    }

    /// Push a `BrokenAnchor` issue when `fragment` is absent from the target file's headings.
    /// `canonical` is the file whose headings to check — the resolved target, or the source
    /// itself for self-anchor links (`[[#section]]`).
    fn validate_fragment(
        &mut self,
        link: &Link,
        source_path: &CanonicalPath,
        canonical: &CanonicalPath,
        fragment: &str,
    ) {
        let headings = self.headings_for(canonical);
        // Wiki/cross-repo fragments also accept raw heading text (Obsidian convention) —
        // slugify before lookup. Standard links stay slug-exact: renderers resolve them as
        // real URL fragments, where raw text genuinely 404s.
        let ok = headings.contains(fragment)
            || (!matches!(link.kind, LinkKind::Standard) && headings.contains(&anchor_id(fragment)));
        if !ok {
            self.issues.push(LinkIssue {
                link: link.target.clone(),
                source: source_path.to_string(),
                kind: IssueKind::BrokenAnchor(fragment.to_string()),
            });
        }
    }

    fn emit_style_if_needed(
        &mut self,
        link: &Link,
        source: &CanonicalPath,
        canonical: &CanonicalPath,
        repo_root: &CanonicalPath,
        by_name: &HashMap<String, Vec<String>>,
        scope: StyleScope,
    ) {
        let Some(rel_target) =
            rel_path(canonical.as_str(), repo_root.as_str()).map(|s| s.to_string())
        else {
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

    fn enqueue_resolved(&mut self, canonical: CanonicalPath, owning_root: CanonicalPath) {
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
        let snapshot: Vec<(CanonicalPath, HashSet<String>)> = self
            .indices
            .iter()
            .map(|(r, i)| (r.clone(), i.md_files.clone()))
            .collect();
        for (root, keep) in snapshot {
            self.cache.prune(root.as_ref(), &keep);
        }
    }
}

// MARK: - --fix byte rewriter

/// Rewrite the path bytes inside `[[...]]` or `` `...` `` for every `.style` issue.
/// Replacements per source file are applied in descending byte-offset order so earlier
/// offsets stay valid. Atomic write via `tempfile`.
///
/// Returns the number of source files that could not be read or rewritten — non-zero means
/// some reported fixes did NOT land, and the caller must fail the run.
pub fn apply_style_fixes(issues: &[LinkIssue]) -> usize {
    let mut failures = 0;
    let mut by_source: HashMap<String, Vec<&LinkIssue>> = HashMap::new();
    for i in issues {
        by_source.entry(i.source.clone()).or_default().push(i);
    }
    for (source, mut source_issues) in by_source {
        let data = match fs::read(&source) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("md-orphan: error: cannot read {source} for --fix: {e}");
                failures += 1;
                continue;
            }
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
                && path_start <= path_end && *path_end <= bytes.len() {
                    bytes.splice(*path_start..*path_end, suggested.as_bytes().iter().copied());
                }
        }
        if let Err(e) = atomic_write_bytes(Path::new(&source), &bytes) {
            eprintln!("md-orphan: error: cannot apply --fix to {source}: {e}");
            failures += 1;
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::write;
    use std::fs;
    use tempfile::TempDir;

    /// Default-options crawl of `entry` (relative to `root`), extraction cache off.
    fn crawl(root: &Path, entry: &str) -> (HashSet<CanonicalPath>, Vec<LinkIssue>) {
        let mut cache = ExtractionCache::for_tests(false);
        let (_, reachable, issues) = bfs_crawl_at_root(
            &[root.join(entry).to_string_lossy().to_string()],
            root.to_str().unwrap(),
            &CrawlOptions::new(),
            &mut cache,
        );
        (reachable, issues)
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
    fn fold_segments_lenient_collapses_dot_dot() {
        assert_eq!(fold_segments("/a/b/../c", false), ("/a/c".into(), false));
        assert_eq!(fold_segments("/a/./b", false), ("/a/b".into(), false));
    }

    #[test]
    fn fold_segments_lenient_absorbs_dot_dot_past_root() {
        // Climbing above root is a silent no-op; re-entry resolves. (Same-repo containment is
        // enforced by resolve_link's final prefix check, not here.)
        assert_eq!(fold_segments("/a/b/../../../a/b/c.md", false), ("/a/b/c.md".into(), false));
    }

    #[test]
    fn fold_segments_strict_escapes_on_dot_dot_past_root() {
        // Cross-repo mode: the same re-entering path is rejected outright as escaped.
        let (_, escaped) = fold_segments("/a/b/../../../a/b/c.md", true);
        assert!(escaped);
    }

    #[test]
    fn fold_segments_strict_no_escape_within_root() {
        // `..` that stays within the path never trips the strict escape.
        assert_eq!(fold_segments("/a/b/c/../d", true), ("/a/b/d".into(), false));
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
        let (_, issues) = crawl(&canonical, "index.md");
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
        let (reachable, issues) = crawl(&canonical, "index.md");
        assert!(issues.is_empty());
        assert_eq!(reachable.len(), 2);
    }

    #[test]
    fn basename_fallback() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[a](guide.md)");
        write(&canonical.join("docs/guide.md"), "hello");
        let (reachable, issues) = crawl(&canonical, "index.md");
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
        let (_, issues) = crawl(&canonical, "index.md");
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
        let (_, issues) = crawl(&canonical, "index.md");
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
    fn self_anchor_valid() {
        // `[[#section]]` pointing at a heading in the same file — no issue.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(
            &canonical.join("index.md"),
            "# Intro\n\njump to [[#details]]\n\n## Details\n",
        );
        let (_, issues) = crawl(&canonical, "index.md");
        assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
    }

    #[test]
    fn self_anchor_broken() {
        // `[[#nope]]` with no matching heading in the same file → BrokenAnchor.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(
            &canonical.join("index.md"),
            "# Intro\n\njump to [[#nope]]\n\n## Details\n",
        );
        let (_, issues) = crawl(&canonical, "index.md");
        let anchors: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::BrokenAnchor(_)))
            .collect();
        assert_eq!(anchors.len(), 1, "expected 1 BrokenAnchor, got: {:?}", issues);
        if let IssueKind::BrokenAnchor(frag) = &anchors[0].kind {
            assert_eq!(frag, "nope");
        }
    }

    #[test]
    fn raw_text_anchor_in_wiki_link() {
        // `[[other.md#Existing Section]]` — raw heading text, not the slug. Accepted for
        // wiki links (Obsidian convention): slugified before lookup.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[[other.md#Existing Section]]");
        write(&canonical.join("other.md"), "# Existing Section\n\nSome content");
        let (_, issues) = crawl(&canonical, "index.md");
        assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
    }

    #[test]
    fn raw_text_self_anchor() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(
            &canonical.join("index.md"),
            "# Intro\n\njump to [[#Details Section]]\n\n## Details Section\n",
        );
        let (_, issues) = crawl(&canonical, "index.md");
        assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
    }

    #[test]
    fn raw_text_anchor_in_standard_link_still_broken() {
        // Standard links resolve as real URL fragments on GitHub — raw heading text 404s
        // there, so leniency does NOT apply. Slug-exact match required.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "[ref](other.md#Existing Section)");
        write(&canonical.join("other.md"), "# Existing Section\n\nSome content");
        let (_, issues) = crawl(&canonical, "index.md");
        let anchors: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::BrokenAnchor(_)))
            .collect();
        assert_eq!(anchors.len(), 1, "expected 1 BrokenAnchor, got: {:?}", issues);
        if let IssueKind::BrokenAnchor(frag) = &anchors[0].kind {
            assert_eq!(frag, "Existing Section");
        }
    }

    #[test]
    fn plain_backtick_span_is_silent_and_not_followed() {
        // `path.ext` with no `(repo)` suffix is prose — no issue, no reachability.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "see `other.md` and `nope.md`");
        write(&canonical.join("other.md"), "hello");
        let (reachable, issues) = crawl(&canonical, "index.md");
        assert!(issues.is_empty(), "expected silence, got: {:?}", issues);
        assert_eq!(reachable.len(), 1, "backtick span must not mark other.md reachable");
    }

    /// Crawl with `include_all_extensions` on (and the extraction cache off).
    fn crawl_all_ext(root: &Path, entry: &str) -> (HashSet<CanonicalPath>, Vec<LinkIssue>) {
        let opts = CrawlOptions {
            use_walk_cache: false,
            include_all_extensions: true,
            ..CrawlOptions::new()
        };
        let mut cache = ExtractionCache::for_tests(false);
        let (_, reachable, issues) = bfs_crawl_at_root(
            &[root.join(entry).to_string_lossy().to_string()],
            root.to_str().unwrap(),
            &opts,
            &mut cache,
        );
        (reachable, issues)
    }

    #[test]
    fn non_md_wiki_link_broken_without_all_extensions() {
        // by_name is .md-only by default, so [[foo.cs]] from another dir can't resolve.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("docs/index.md"), "see [[foo.cs]]");
        write(&canonical.join("src/foo.cs"), "");
        let (_, issues) = crawl(&canonical, "docs/index.md");
        assert_eq!(issues.len(), 1, "expected Broken, got: {:?}", issues);
        assert!(matches!(issues[0].kind, IssueKind::Broken));
    }

    #[test]
    fn non_md_wiki_link_resolves_with_all_extensions() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("docs/index.md"), "see [[foo.cs]]");
        write(&canonical.join("src/foo.cs"), "");
        let (_, issues) = crawl_all_ext(&canonical, "docs/index.md");
        assert!(issues.is_empty(), "expected clean resolve, got: {:?}", issues);
    }

    #[test]
    fn wiki_style_relative_path_flagged() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("docs/dev/index.md"), "see [[../system/foo.md]]");
        write(&canonical.join("docs/system/foo.md"), "");
        let (_, issues) = crawl(&canonical, "docs/dev/index.md");
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
    fn cross_repo_annotation_with_unknown_name_is_silent() {
        // `foo.md` (no-such-repo) — looks like a cross-repo ref but the name isn't a configured
        // repo, so the parser treats it as plain inline code and emits no link.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        write(&canonical.join("index.md"), "see `foo.md` (no-such-repo)");
        let (_, issues) = crawl(&canonical, "index.md");
        // No issues at all — annotation is silent (parser pre-filter), and the file has
        // no other links to resolve.
        assert!(issues.is_empty());
    }

    /// Cross-repo crawl harness: entry repo with `index.md` = `entry_content`, plus a
    /// configured `repo_name` pointing at a second tempdir populated with `target_files`.
    fn cross_repo_crawl(
        entry_content: &str,
        repo_name: &str,
        target_files: &[(&str, &str)],
    ) -> Vec<LinkIssue> {
        let entry_dir = TempDir::new().unwrap();
        let target_dir = TempDir::new().unwrap();
        let entry = fs::canonicalize(entry_dir.path()).unwrap();
        let target = fs::canonicalize(target_dir.path()).unwrap();
        write(&entry.join(".md-orphan"), "");
        write(&entry.join("index.md"), entry_content);
        write(&target.join(".md-orphan"), "");
        for (rel, content) in target_files {
            write(&target.join(rel), content);
        }
        let opts = CrawlOptions {
            repos: HashMap::from([(repo_name.to_string(), target.to_string_lossy().to_string())]),
            use_walk_cache: false,
            ..CrawlOptions::new()
        };
        let mut cache = ExtractionCache::new(false, opts.repos.keys().cloned().collect());
        let (_, _, issues) = bfs_crawl_at_root(
            &[entry.join("index.md").to_string_lossy().to_string()],
            entry.to_str().unwrap(),
            &opts,
            &mut cache,
        );
        issues
    }

    #[test]
    fn cross_repo_ref_to_known_repo_with_missing_target_flagged() {
        // `(known-repo)` IS configured but the target file doesn't exist → CrossRepoBroken.
        // Confirms the parser-side filter doesn't mask real cross-repo errors.
        let issues = cross_repo_crawl("see `missing.md` (known-repo)", "known-repo", &[]);
        let broken: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::CrossRepoBroken(_)))
            .collect();
        assert_eq!(broken.len(), 1, "expected 1 CrossRepoBroken, got: {:?}", issues);
    }

    #[test]
    fn cross_repo_files_internal_broken_links_not_reported() {
        // Entry repo references a file in target repo; that file has its own broken link.
        // The internal broken link is NOT meow-tower's responsibility — should be silent.
        let issues = cross_repo_crawl(
            "see `outgoing.md` (target-repo)",
            "target-repo",
            &[("outgoing.md", "[bad](nonexistent.md) intra-target broken link")],
        );
        assert!(issues.is_empty(), "expected silent, got: {:?}", issues);
    }

    #[test]
    fn entry_repo_anchor_check_on_cross_repo_target_still_works() {
        // Anchor verification on entry-repo's outgoing refs must still read the target file's
        // headings. Skipping recursion ≠ skipping anchor verification on direct refs.
        let issues = cross_repo_crawl(
            "see `outgoing.md#nope` (target-repo)",
            "target-repo",
            &[("outgoing.md", "# Real Section\n")],
        );
        let anchors: Vec<_> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::BrokenAnchor(_)))
            .collect();
        assert_eq!(anchors.len(), 1, "expected 1 BrokenAnchor, got: {:?}", issues);
    }

    #[test]
    fn cross_repo_ref_to_locally_missing_repo_is_silent() {
        // Repo configured but its path doesn't exist on this machine — refs to it should be
        // dropped at the parser layer, same as unknown-name annotations.
        let entry_dir = TempDir::new().unwrap();
        let entry = fs::canonicalize(entry_dir.path()).unwrap();
        write(&entry.join(".md-orphan"), "");
        write(&entry.join("index.md"), "see `foo.md` (some-repo)");
        let mut cfg = crate::config::GlobalConfig {
            repos: HashMap::from([(
                "some-repo".to_string(),
                "/tmp/__md_orphan_definitely_not_a_dir__".to_string(),
            )]),
        };
        cfg.retain_existing();
        // After retain, the missing repo is gone.
        assert!(cfg.repos.is_empty());

        let opts = CrawlOptions {
            repos: cfg.repos,
            use_walk_cache: false,
            ..CrawlOptions::new()
        };
        let mut cache = ExtractionCache::new(false, opts.repos.keys().cloned().collect());
        let (_, _, issues) = bfs_crawl_at_root(
            &[entry.join("index.md").to_string_lossy().to_string()],
            entry.to_str().unwrap(),
            &opts,
            &mut cache,
        );
        assert!(issues.is_empty(), "expected silent, got: {:?}", issues);
    }

    #[test]
    fn fix_rewrites_wiki_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        let entry = canonical.join("a/index.md");
        write(&entry, "x [[docs/guide.md]] y");
        write(&canonical.join("a/docs/guide.md"), "");
        let (_, issues) = crawl(&canonical.join("a"), "index.md");
        let style: Vec<&LinkIssue> = issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Style { .. }))
            .collect();
        assert_eq!(style.len(), 1);

        // Apply fix.
        let style_owned: Vec<LinkIssue> = style.into_iter().cloned().collect();
        assert_eq!(apply_style_fixes(&style_owned), 0);

        let rewritten = fs::read_to_string(&entry).unwrap();
        assert_eq!(rewritten, "x [[guide.md]] y");

        // Idempotency: re-run yields no style issues.
        let (_, issues2) = crawl(&canonical.join("a"), "index.md");
        let style2: Vec<_> = issues2
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Style { .. }))
            .collect();
        assert!(style2.is_empty());
    }

    #[test]
    fn unreadable_entry_reports_issue_not_orphan() {
        // A seeded entry that can't be read → Unreadable issue; it still lands in the
        // reachable set (marked before the read) so it can't double-report as an orphan.
        let dir = TempDir::new().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        let missing = canonical.join("gone.md");
        let (reachable, issues) = crawl(&canonical, "gone.md");
        assert_eq!(issues.len(), 1, "expected 1 Unreadable, got: {:?}", issues);
        assert!(matches!(issues[0].kind, IssueKind::Unreadable));
        assert_eq!(issues[0].source, missing.to_string_lossy());
        assert!(reachable.contains(missing.to_string_lossy().as_ref()));
    }

    #[test]
    fn fix_counts_unreadable_source_as_failure() {
        // A Style issue whose source file vanished before --fix → read fails → counted.
        let issue = LinkIssue {
            link: "docs/guide.md".into(),
            source: "/nonexistent/__md_orphan_test__/index.md".into(),
            kind: IssueKind::Style {
                scope: StyleScope::Wiki,
                suggested: "guide.md".into(),
                path_start: 2,
                path_end: 15,
            },
        };
        assert_eq!(apply_style_fixes(&[issue]), 1);
    }
}
