//! Repo discovery: `index_repo` walks the tree and builds a `RepoIndex`.
//!
//! Uses `ignore::WalkParallel` (the same walker behind `fd`/`ripgrep`). Work-stealing
//! across N threads; we disable ignore's `.gitignore` parsing — we have our own
//! `ExcludeMatcher` driven by built-in defaults + per-repo `.md-orphan`.
//!
//! `standard_filters(false)` and `hidden(false)` keep ignore from second-guessing our
//! exclude semantics. Dot-dir skipping happens inside our visitor.

use crate::exclude::{ExcludeMatcher, DEFAULT_EXCLUDES};
use crate::path::real_path;
use ignore::{WalkBuilder, WalkState};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoIndex {
    /// Canonical (realpath'd) absolute root.
    pub root: PathBuf,
    /// Relative paths of `.md` files — for orphan reachability tracking.
    pub md_files: HashSet<String>,
    /// Basename → list of absolute paths. `.md` only unless `include_all_extensions`.
    pub by_name: HashMap<String, Vec<String>>,
    /// Effective exclude patterns applied (defaults + user).
    pub exclude: Vec<String>,
}

/// Walk a repo, applying exclude pruning at the visitor stage so subtrees skip without iteration.
/// Bare-basename excludes hit the `HashSet::contains` fast path on the dir's basename — no
/// rel_path allocation. Anchored/glob patterns fall through to the slow path.
///
/// Parallelism: `ignore` uses a work-stealing thread pool (default = num_cpus); each thread
/// has its own `read_dir` loop. Per-thread accumulators merged into shared `Mutex` at end-of-fn.
///
/// `include_all_extensions=true` populates `by_name` for every file, not just `.md`. ~30× the
/// work on Unity-sized repos — leave off unless non-`.md` style support actually fires.
pub fn index_repo(
    root: &str,
    exclude: &[String],
    use_default_excludes: bool,
    include_all_extensions: bool,
) -> RepoIndex {
    let resolved = real_path(root).unwrap_or_else(|| PathBuf::from(root));
    let mut effective: Vec<String> = if use_default_excludes {
        DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };
    effective.extend(exclude.iter().cloned());
    let matcher = ExcludeMatcher::new(effective.clone());
    let bare_only = matcher.is_bare_only();
    let resolved_str = resolved.to_string_lossy().to_string();
    let root_prefix = format!("{}/", resolved_str);

    // Shared accumulators. Lock contention is small at our entry counts (~15k post-prune)
    // because each insert is microseconds and locked sections are tight.
    let md_files: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    let by_name: Mutex<HashMap<String, Vec<String>>> = Mutex::new(HashMap::new());

    let walker = WalkBuilder::new(&resolved)
        .standard_filters(false) // ← we have our own ExcludeMatcher; no .gitignore parsing
        .hidden(false)
        .follow_links(false) // match Swift-era FTS_PHYSICAL behavior
        .build_parallel();

    walker.run(|| {
        // Per-thread closure: capture by reference where possible, clone strings as needed.
        let matcher = &matcher;
        let md_files = &md_files;
        let by_name = &by_name;
        let root_prefix = root_prefix.as_str();

        Box::new(move |result| {
            let entry = match result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            let path = entry.path();
            let depth = entry.depth();

            // Determine type once. ignore::DirEntry::file_type() returns Option<FileType>;
            // for `.` (root) it's Some(dir). filter_entry equivalent lives inline here.
            let ft = match entry.file_type() {
                Some(t) => t,
                None => return WalkState::Continue,
            };

            // Directory pruning. Root passes through (depth 0).
            if ft.is_dir() {
                if depth == 0 {
                    return WalkState::Continue;
                }
                let name_cow = entry.file_name().to_string_lossy();
                let name: &str = &name_cow;
                // Dot-dirs at any depth.
                if name.starts_with('.') && name.len() > 1 {
                    return WalkState::Skip;
                }
                if matcher.matches_bare(name) {
                    return WalkState::Skip;
                }
                if bare_only {
                    return WalkState::Continue;
                }
                let abs = path.to_string_lossy();
                if let Some(rel) = abs.strip_prefix(root_prefix) {
                    if matcher.matches(rel, Some(name)) {
                        return WalkState::Skip;
                    }
                }
                return WalkState::Continue;
            }

            // File / symlink processing. ignore yields entries via DT_LNK/DT_REG (no extra stat).
            if !(ft.is_file() || ft.is_symlink()) {
                return WalkState::Continue;
            }
            let name_cow = entry.file_name().to_string_lossy();
            let name: &str = &name_cow;
            let is_md = name.len() >= 3 && name.as_bytes().ends_with(b".md");

            if !is_md && !include_all_extensions {
                return WalkState::Continue;
            }
            let abs = path.to_string_lossy().to_string();
            let rel = match abs.strip_prefix(root_prefix) {
                Some(r) => r.to_string(),
                None => abs.clone(),
            };
            if matcher.matches(&rel, Some(name)) {
                return WalkState::Continue;
            }
            // Mutex-locked inserts. Could go thread-local + merge for hotter workloads;
            // at our scale (single-digit ms total locked) it's not the bottleneck.
            by_name.lock().unwrap().entry(name.to_string()).or_default().push(abs);
            if is_md {
                md_files.lock().unwrap().insert(rel);
            }
            WalkState::Continue
        })
    });

    RepoIndex {
        root: resolved,
        md_files: md_files.into_inner().unwrap(),
        by_name: by_name.into_inner().unwrap(),
        exclude: effective,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &std::path::Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn discovers_md_files() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("a.md"), "");
        write(&dir.path().join("docs/b.md"), "");
        write(&dir.path().join("notes.txt"), ""); // ignored: not .md

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false);
        assert!(idx.md_files.contains("a.md"));
        assert!(idx.md_files.contains("docs/b.md"));
        assert_eq!(idx.md_files.len(), 2);
    }

    #[test]
    fn skips_dot_directories() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("a.md"), "");
        write(&dir.path().join(".git/HEAD"), "");
        write(&dir.path().join(".git/foo.md"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false);
        assert!(idx.md_files.contains("a.md"));
        assert_eq!(idx.md_files.len(), 1);
    }

    #[test]
    fn applies_default_excludes_at_any_depth() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("README.md"), "");
        write(&dir.path().join("proj-ios/Pods/Firebase/README.md"), "");
        write(&dir.path().join("Pods/Other/x.md"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false);
        assert!(idx.md_files.contains("README.md"));
        assert_eq!(idx.md_files.len(), 1);
    }

    #[test]
    fn user_excludes_layer_on_top() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("a.md"), "");
        write(&dir.path().join("Packages/inner.md"), "");

        let idx = index_repo(
            dir.path().to_str().unwrap(),
            &["Packages/".to_string()],
            true,
            false,
        );
        assert!(idx.md_files.contains("a.md"));
        assert_eq!(idx.md_files.len(), 1);
    }

    #[test]
    fn no_default_excludes_disables_them() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("Pods/x.md"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], false, false);
        assert!(idx.md_files.contains("Pods/x.md"));
    }

    #[test]
    fn by_name_md_only_by_default() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("foo.md"), "");
        write(&dir.path().join("bar.cs"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false);
        assert!(idx.by_name.contains_key("foo.md"));
        assert!(!idx.by_name.contains_key("bar.cs"));
    }

    #[test]
    fn by_name_includes_all_extensions_when_enabled() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("foo.md"), "");
        write(&dir.path().join("bar.cs"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, true);
        assert!(idx.by_name.contains_key("foo.md"));
        assert!(idx.by_name.contains_key("bar.cs"));
    }
}
