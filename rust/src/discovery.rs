//! Repo discovery: `index_repo` walks the tree and builds a `RepoIndex`.
//!
//! Mirrors `Sources/Lib/Discovery.swift`. Swift used `fts(3)` with `FTS_PHYSICAL | FTS_NOCHDIR
//! | FTS_NOSTAT`. Rust uses `walkdir` — Round-1 research said walkdir is ~3× faster than fts
//! single-threaded. `sort_by_file_name()` for deterministic order across machines (Round-2).
//! `walkdir` always stats; the FTS_NOSTAT optimization is lost but acceptable since walk-cache
//! amortizes cold-walk cost.

use crate::exclude::{ExcludeMatcher, DEFAULT_EXCLUDES};
use crate::path::real_path;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use walkdir::WalkDir;

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

/// Walk a repo, applying exclude pruning at the `walkdir::filter_entry` stage so subtrees skip
/// without iteration. Bare-basename excludes hit the `HashSet::contains` fast path on the dir's
/// basename — no rel_path allocation. Anchored/glob patterns fall through to the slow path.
///
/// `include_all_extensions=true` is for the (deferred) non-`.md` style support — populates
/// `by_name` for every file, not just `.md`. ~30× the work on Unity-sized repos.
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

    let mut md_files: HashSet<String> = HashSet::new();
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();

    let walker = WalkDir::new(&resolved)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            // Always allow files through; pruning happens for directories only.
            if !e.file_type().is_dir() {
                return true;
            }
            // The root itself is a dir but has no parent — keep it.
            if e.depth() == 0 {
                return true;
            }
            let name_cow = e.file_name().to_string_lossy();
            let name: &str = &name_cow;
            // Skip dot-dirs at any depth (.git, .build, .venv, .config, ...).
            if name.starts_with('.') && name.len() > 1 {
                return false;
            }
            // Bare-basename fast path — no rel_path alloc.
            if matcher.matches_bare(name) {
                return false;
            }
            if bare_only {
                return true;
            }
            // Full rel_path needed for anchored/glob patterns.
            let abs = e.path().to_string_lossy();
            if let Some(rel) = abs.strip_prefix(&format!("{}/", resolved_str)) {
                if matcher.matches(rel, Some(name)) {
                    return false;
                }
            }
            true
        });

    for entry in walker.flatten() {
        let ft = entry.file_type();
        if !(ft.is_file() || ft.is_symlink()) {
            continue;
        }
        let name_cow = entry.file_name().to_string_lossy();
        let name: &str = &name_cow;
        let is_md = name.len() >= 3 && name.as_bytes().ends_with(b".md");

        if !is_md && !include_all_extensions {
            continue;
        }
        let abs = entry.path().to_string_lossy().to_string();
        let rel = match abs.strip_prefix(&format!("{}/", resolved_str)) {
            Some(r) => r.to_string(),
            None => abs.clone(),
        };
        // File-level exclude check (pattern targeting files via globs / plain prefixes).
        if matcher.matches(&rel, Some(name)) {
            continue;
        }
        by_name
            .entry(name.to_string())
            .or_insert_with(Vec::new)
            .push(abs);
        if is_md {
            md_files.insert(rel);
        }
    }

    RepoIndex {
        root: resolved,
        md_files,
        by_name,
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
        write(&dir.path().join(".git/foo.md"), ""); // should be pruned

        let idx = index_repo(dir.path().to_str().unwrap(), &[], true, false);
        assert!(idx.md_files.contains("a.md"));
        assert_eq!(idx.md_files.len(), 1);
    }

    #[test]
    fn applies_default_excludes_at_any_depth() {
        // Pods/ default should match "proj-ios/Pods/" too (gitignore semantics).
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
