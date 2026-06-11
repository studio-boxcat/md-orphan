//! Walk-result cache: persist `RepoIndex`, validate via per-dir mtime. See [[architecture.md#persisted-state]].
//!
//! Hit skips the entire `index_repo` walk. Dir mtime bumps on entry add/remove/rename
//! (not file content edits — those are caught by per-file [`ExtractionCache`](crate::cache::ExtractionCache)).

use crate::cache::{atomic_write_json, cache_directory, fnv1a64, fnv1a64_hex, fnv1a64_of_sorted};
use crate::discovery::RepoIndex;
use crate::path::{is_under, real_path, CanonicalPath};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bump on any change that makes a previously-written cache unsafe to consume:
/// - `WalkCache` / `RepoIndex` shape (field add/remove/rename, type change)
/// - `DEFAULT_EXCLUDES` content (in `exclude.rs`)
/// - `ExcludeMatcher` semantics (fnmatch flags, anchored vs nested rules in `exclude.rs`)
/// - `index_repo` filter rules (e.g. the `is_md` byte test, `include_all_extensions` logic)
/// - `ignore` crate semver bump that changes traversal order/visibility
/// - serde format change (e.g. tagged-enum representation drift)
///
/// Files written under a different schema are silently treated as a miss + overwritten.
const WALK_CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WalkCache {
    pub schema_version: u32,
    /// Stored to detect fnv1a64-collision: two distinct paths hashing to the same cache file
    /// would otherwise return a wrong-repo `RepoIndex`.
    pub canonical_root: String,
    pub flags_key: u64,
    /// Relative dir path → mtime_ns. Unchanged dir = no entry add/remove/rename happened in it.
    pub dir_mtimes: HashMap<String, i64>,
    pub md_files: HashSet<String>,
    pub by_name: HashMap<String, Vec<String>>,
    pub effective_exclude: Vec<String>,
}

fn walk_cache_directory() -> PathBuf {
    // `cache_directory()` always ends in "/md-orphan/cache", so a parent always exists.
    cache_directory().parent().unwrap().join("walk-cache")
}

fn walk_cache_file_path(canonical_root: &CanonicalPath) -> PathBuf {
    walk_cache_directory().join(format!("{}.json", fnv1a64_hex(canonical_root.as_str())))
}

/// fnv1a64 of `(use_default_excludes, include_all_extensions, sorted exclude list)`.
/// Sort first so call-site order doesn't cause false misses.
pub(crate) fn compute_flags_key(
    use_default_excludes: bool,
    include_all_extensions: bool,
    exclude: &[String],
) -> u64 {
    let exclude_hash = fnv1a64_of_sorted(exclude.iter().map(String::as_str));
    let prefix = format!(
        "{}|{}|",
        if use_default_excludes { '1' } else { '0' },
        if include_all_extensions { '1' } else { '0' },
    );
    // Combine fixed-width flag prefix with exclude-list hash. Mixing into a single fnv1a64 input
    // keeps this stable across exclude reordering (since the inner hash already sorts).
    let mut buf = prefix.into_bytes();
    buf.extend_from_slice(&exclude_hash.to_le_bytes());
    fnv1a64(&buf)
}

/// Load + validate a walk cache for `canonical_root`. Returns `Some(RepoIndex)` on a verified
/// hit, `None` on any miss/error/staleness.
pub(crate) fn try_load_walk_cache(
    canonical_root: &CanonicalPath,
    flags_key: u64,
) -> Option<RepoIndex> {
    let path = walk_cache_file_path(canonical_root);
    let raw = fs::read_to_string(&path).ok()?;
    let cache: WalkCache = serde_json::from_str(&raw).ok()?;
    if cache.schema_version != WALK_CACHE_SCHEMA_VERSION {
        return None;
    }
    if cache.canonical_root != canonical_root.as_str() {
        return None;
    }
    if cache.flags_key != flags_key {
        return None;
    }
    let root: &Path = canonical_root.as_ref();
    for (rel_dir, expected_mtime_ns) in &cache.dir_mtimes {
        let abs = if rel_dir.is_empty() {
            root.to_path_buf()
        } else {
            root.join(rel_dir)
        };
        if stat_mtime_ns(&abs)? != *expected_mtime_ns {
            return None;
        }
    }
    Some(RepoIndex {
        root: canonical_root.clone(),
        md_files: cache.md_files,
        by_name: cache.by_name,
        exclude: cache.effective_exclude,
    })
}

pub(crate) fn save_walk_cache(
    canonical_root: &CanonicalPath,
    flags_key: u64,
    index: &RepoIndex,
    dir_mtimes: HashMap<String, i64>,
) {
    // A cache dir under the indexed root (custom XDG inside the tree, or indexing $HOME with
    // a non-default layout) would have its parent-dir mtimes bumped by this very save —
    // guaranteeing a miss next run. Skip persisting; the walk itself stays correct.
    let cache_dir = canonicalize_best_effort(&walk_cache_directory());
    if is_under(&cache_dir.to_string_lossy(), canonical_root.as_str()) {
        return;
    }
    let cache = WalkCache {
        schema_version: WALK_CACHE_SCHEMA_VERSION,
        canonical_root: canonical_root.as_str().to_owned(),
        flags_key,
        dir_mtimes,
        md_files: index.md_files.clone(),
        by_name: index.by_name.clone(),
        effective_exclude: index.exclude.clone(),
    };
    atomic_write_json(&walk_cache_file_path(canonical_root), &cache, "walk-cache");
}

/// realpath(3) of the nearest existing ancestor, with the missing tail re-appended.
/// The cache dir may not exist yet (first run), but its containment check still has to see
/// through symlinks (macOS `/var` → `/private/var`).
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Some(p) = real_path(path) {
        return p;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        return canonicalize_best_effort(parent).join(name);
    }
    path.to_path_buf()
}

pub(crate) fn stat_mtime_ns(path: &Path) -> Option<i64> {
    let meta = fs::symlink_metadata(path).ok()?;
    let dur = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    i64::try_from(dur.as_nanos()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{xdg_test_env, xdg_test_lock};
    use crate::path::canonicalize_str;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn flags_key_stable_across_reordering() {
        let a = compute_flags_key(true, false, &["b".into(), "a".into()]);
        let b = compute_flags_key(true, false, &["a".into(), "b".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn flags_key_changes_on_default_excludes_flip() {
        let a = compute_flags_key(true, false, &["a".into()]);
        let b = compute_flags_key(false, false, &["a".into()]);
        assert_ne!(a, b);
    }

    #[test]
    fn flags_key_changes_on_pattern_change() {
        let a = compute_flags_key(true, false, &["a".into()]);
        let b = compute_flags_key(true, false, &["b".into()]);
        assert_ne!(a, b);
    }

    #[test]
    fn cache_hit_returns_repo_index() {
        let _g = xdg_test_lock();
        let (_tmp, _, canon) = xdg_test_env();
        let canon = canonicalize_str(&canon).unwrap();

        let mut md = HashSet::new();
        md.insert("a.md".to_string());
        let index = RepoIndex {
            root: canon.clone(),
            md_files: md,
            by_name: HashMap::new(),
            exclude: vec!["Library/".to_string()],
        };
        let mut dir_mtimes = HashMap::new();
        dir_mtimes.insert("".into(), stat_mtime_ns(canon.as_ref()).unwrap());

        let key = compute_flags_key(true, false, &["Library/".into()]);
        save_walk_cache(&canon, key, &index, dir_mtimes);

        let loaded = try_load_walk_cache(&canon, key).expect("hit");
        assert_eq!(loaded.md_files, index.md_files);
    }

    #[test]
    fn cache_miss_on_flags_change() {
        let _g = xdg_test_lock();
        let (_tmp, _, canon) = xdg_test_env();
        let canon = canonicalize_str(&canon).unwrap();

        let index = RepoIndex {
            root: canon.clone(),
            md_files: HashSet::new(),
            by_name: HashMap::new(),
            exclude: Vec::new(),
        };
        let mut dir_mtimes = HashMap::new();
        dir_mtimes.insert("".into(), stat_mtime_ns(canon.as_ref()).unwrap());

        let key_a = compute_flags_key(true, false, &[]);
        save_walk_cache(&canon, key_a, &index, dir_mtimes);

        let key_b = compute_flags_key(false, false, &[]); // flag flipped → miss
        assert!(try_load_walk_cache(&canon, key_b).is_none());
    }

    #[test]
    fn save_refused_when_cache_dir_inside_root() {
        // XDG resolving under the indexed root → persisting would self-invalidate. Refuse.
        let _g = xdg_test_lock();
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("xdg")).unwrap();
        // SAFETY: caller holds `xdg_test_lock()`; env is process-global but serialized.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", repo.join("xdg")) };
        let canon = canonicalize_str(&repo).unwrap();

        let index = RepoIndex {
            root: canon.clone(),
            md_files: HashSet::new(),
            by_name: HashMap::new(),
            exclude: Vec::new(),
        };
        save_walk_cache(&canon, 1, &index, HashMap::new());
        assert!(
            !repo.join("xdg/md-orphan/walk-cache").exists(),
            "expected save to be refused for in-root cache dir"
        );
    }

    #[test]
    fn cache_miss_on_dir_mtime_change() {
        let _g = xdg_test_lock();
        let (_tmp, _, canon) = xdg_test_env();
        let canon = canonicalize_str(&canon).unwrap();

        let index = RepoIndex {
            root: canon.clone(),
            md_files: HashSet::new(),
            by_name: HashMap::new(),
            exclude: Vec::new(),
        };
        let mut dir_mtimes = HashMap::new();
        // Mtime that won't match real fs — verifies validation catches drift.
        dir_mtimes.insert("".into(), 1_700_000_000_000_000_000);

        let key = compute_flags_key(true, false, &[]);
        save_walk_cache(&canon, key, &index, dir_mtimes);

        assert!(try_load_walk_cache(&canon, key).is_none());
    }
}
