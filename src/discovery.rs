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
use crate::walk_cache::{compute_flags_key, save_walk_cache, stat_mtime_ns, try_load_walk_cache};
use ignore::{WalkBuilder, WalkState};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    use_walk_cache: bool,
) -> RepoIndex {
    let resolved = real_path(root).unwrap_or_else(|| PathBuf::from(root));
    let mut effective: Vec<String> = if use_default_excludes {
        DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };
    effective.extend(exclude.iter().cloned());

    // See [[walk_cache.rs]] for hit semantics.
    let flags_key = if use_walk_cache {
        Some(compute_flags_key(use_default_excludes, include_all_extensions, &effective))
    } else {
        None
    };
    if let Some(key) = flags_key {
        if let Some(cached) = try_load_walk_cache(&resolved, key) {
            return cached;
        }
    }

    let matcher = ExcludeMatcher::new(effective.clone());
    let bare_only = matcher.is_bare_only();
    let resolved_str = resolved.to_string_lossy().to_string();
    let root_prefix = format!("{}/", resolved_str);

    // Shared accumulators. Lock contention is small at our entry counts (~15k post-prune)
    // because each insert is microseconds and locked sections are tight.
    let md_files: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    let by_name: Mutex<HashMap<String, Vec<String>>> = Mutex::new(HashMap::new());
    let dir_mtimes: Mutex<HashMap<String, i64>> = Mutex::new(HashMap::new());
    // A walked-but-unstat'd dir would let mtime drift slip past validation. Skip save in that case.
    let mtime_stat_failed = AtomicBool::new(false);

    let walker = WalkBuilder::new(&resolved)
        .standard_filters(false) // ← we have our own ExcludeMatcher; no .gitignore parsing
        .hidden(false)
        .follow_links(false) // match Swift-era FTS_PHYSICAL behavior
        .build_parallel();

    walker.run(|| {
        let matcher = &matcher;
        let md_files = &md_files;
        let by_name = &by_name;
        let dir_mtimes = &dir_mtimes;
        let mtime_stat_failed = &mtime_stat_failed;
        let root_prefix = root_prefix.as_str();

        // Stat & record a kept dir's mtime; on stat failure, flag the run so save is skipped.
        let record_dir_mtime = move |key: String, path: &Path| {
            match stat_mtime_ns(path) {
                Some(ns) => { dir_mtimes.lock().unwrap().insert(key, ns); }
                None => { mtime_stat_failed.store(true, Ordering::Relaxed); }
            }
        };

        Box::new(move |result| {
            let entry = match result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            let path = entry.path();
            let depth = entry.depth();
            let ft = match entry.file_type() {
                Some(t) => t,
                None => return WalkState::Continue,
            };

            if ft.is_dir() {
                if depth == 0 {
                    if use_walk_cache { record_dir_mtime(String::new(), path); }
                    return WalkState::Continue;
                }
                let name_cow = entry.file_name().to_string_lossy();
                let name: &str = &name_cow;
                if name.starts_with('.') && name.len() > 1 {
                    return WalkState::Skip;
                }
                if matcher.matches_bare(name) {
                    return WalkState::Skip;
                }
                let abs = path.to_string_lossy();
                let rel = abs.strip_prefix(root_prefix);
                if !bare_only && let Some(r) = rel && matcher.matches(r, Some(name)) {
                    return WalkState::Skip;
                }
                // Defensive: skip record if path isn't under root_prefix; otherwise a stray absolute
                // key would let `Path::join` on load reroute the stat to an arbitrary path.
                if use_walk_cache && let Some(r) = rel {
                    record_dir_mtime(r.to_string(), path);
                }
                return WalkState::Continue;
            }

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
            by_name.lock().unwrap().entry(name.to_string()).or_default().push(abs);
            if is_md {
                md_files.lock().unwrap().insert(rel);
            }
            WalkState::Continue
        })
    });

    let index = RepoIndex {
        root: resolved.clone(),
        md_files: md_files.into_inner().unwrap(),
        by_name: by_name.into_inner().unwrap(),
        exclude: effective,
    };
    if let Some(key) = flags_key
        && !mtime_stat_failed.load(Ordering::Relaxed)
    {
        save_walk_cache(&resolved, key, &index, dir_mtimes.into_inner().unwrap());
    }
    index
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

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false, false);
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

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false, false);
        assert!(idx.md_files.contains("a.md"));
        assert_eq!(idx.md_files.len(), 1);
    }

    #[test]
    fn applies_default_excludes_at_any_depth() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("README.md"), "");
        write(&dir.path().join("proj-ios/Pods/Firebase/README.md"), "");
        write(&dir.path().join("Pods/Other/x.md"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false, false);
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
            false,
        );
        assert!(idx.md_files.contains("a.md"));
        assert_eq!(idx.md_files.len(), 1);
    }

    #[test]
    fn no_default_excludes_disables_them() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("Pods/x.md"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], false, false, false);
        assert!(idx.md_files.contains("Pods/x.md"));
    }

    #[test]
    fn by_name_md_only_by_default() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("foo.md"), "");
        write(&dir.path().join("bar.cs"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false, false);
        assert!(idx.by_name.contains_key("foo.md"));
        assert!(!idx.by_name.contains_key("bar.cs"));
    }

    #[test]
    fn by_name_includes_all_extensions_when_enabled() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("foo.md"), "");
        write(&dir.path().join("bar.cs"), "");

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, true, false);
        assert!(idx.by_name.contains_key("foo.md"));
        assert!(idx.by_name.contains_key("bar.cs"));
    }

    /// Set up an isolated XDG dir + sibling repo dir under a single tempdir, so cache writes to
    /// `<tmp>/xdg/md-orphan/walk-cache/` don't bump the indexed `<tmp>/repo/` mtimes.
    fn setup_walk_cache_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&xdg).unwrap();
        fs::create_dir_all(&repo).unwrap();
        // SAFETY: caller holds `xdg_test_lock`; env is process-global but serialized.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &xdg); }
        let canon_repo = fs::canonicalize(&repo).unwrap();
        (tmp, xdg, canon_repo)
    }

    #[test]
    fn walk_cache_round_trips_and_returns_same_repo_index() {
        let _g = crate::cache::xdg_test_lock();
        let (_tmp, xdg, repo) = setup_walk_cache_env();
        write(&repo.join("a.md"), "");
        write(&repo.join("docs/b.md"), "");

        let cold = index_repo(repo.to_str().unwrap(), &[], true, false, true);
        assert_eq!(cold.md_files.len(), 2);

        let cache_dir = xdg.join("md-orphan/walk-cache");
        let cache_files: Vec<_> =
            fs::read_dir(&cache_dir).unwrap().filter_map(Result::ok).collect();
        assert_eq!(cache_files.len(), 1, "cold run should write exactly one walk-cache file");

        let warm = index_repo(repo.to_str().unwrap(), &[], true, false, true);
        assert_eq!(cold.md_files, warm.md_files);
        assert_eq!(cold.by_name, warm.by_name);
        assert_eq!(cold.exclude, warm.exclude);
    }

    #[test]
    fn walk_cache_invalidates_on_dir_mutation() {
        let _g = crate::cache::xdg_test_lock();
        let (_tmp, _xdg, repo) = setup_walk_cache_env();
        write(&repo.join("a.md"), "");

        let first = index_repo(repo.to_str().unwrap(), &[], true, false, true);
        assert_eq!(first.md_files.len(), 1);

        // Sleep so APFS dir mtime advances measurably even on coarse-resolution fs.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&repo.join("b.md"), "");

        let second = index_repo(repo.to_str().unwrap(), &[], true, false, true);
        assert!(second.md_files.contains("b.md"));
        assert_eq!(second.md_files.len(), 2);
    }

    #[test]
    fn walk_cache_invalidates_on_flags_change() {
        let _g = crate::cache::xdg_test_lock();
        let (_tmp, _xdg, repo) = setup_walk_cache_env();
        write(&repo.join("a.md"), "");
        write(&repo.join("Packages/inner.md"), "");

        // First run: Packages excluded → 1 file.
        let with_excl = index_repo(
            repo.to_str().unwrap(),
            &["Packages/".to_string()],
            true,
            false,
            true,
        );
        assert_eq!(with_excl.md_files.len(), 1);

        // Second run: same dir, no exclude → flags_key differs → cache miss → re-walk.
        let without_excl = index_repo(repo.to_str().unwrap(), &[], true, false, true);
        assert!(without_excl.md_files.contains("Packages/inner.md"));
        assert_eq!(without_excl.md_files.len(), 2);
    }
}
