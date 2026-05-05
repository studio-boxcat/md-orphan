//! Exclude-pattern matching: built-in defaults + per-repo `.md-orphan`.
//!
//! Mirrors `Sources/Lib/Util.swift` `ExcludeMatcher` + `defaultExcludes` const.
//!
//! Pattern semantics (gitignore-flavored):
//! - Trailing `/` makes it a directory pattern.
//! - **Bare basename + trailing `/`** (`Pods/`, `Library/`) matches at any depth.
//! - **Path-containing + trailing `/`** (`docs/internal/`) is anchored at the repo root.
//! - Patterns with `*`, `?`, `[…]` are matched as `fnmatch(3)` globs (PATHNAME mode).
//! - Plain patterns (no `/`, no glob) match as path prefix at root.

use std::collections::HashSet;
use std::ffi::CString;
use std::os::raw::{c_char, c_int};

/// Built-in default excludes — directories big, auto-generated, or never doc trees.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/",
    ".svn/",
    ".hg/",
    "node_modules/",
    ".build/",
    "DerivedData/",
    "Library/",
    "Pods/",
    "target/",
    "vendor/",
    ".venv/",
    "__pycache__/",
];

unsafe extern "C" {
    fn fnmatch(pattern: *const c_char, name: *const c_char, flags: c_int) -> c_int;
}
const FNM_PATHNAME: c_int = 0x2;

/// Pre-compiled exclude matcher. Built once per `index_repo` call; consulted ~4k+ times during
/// the walk. Splits patterns into the four forms so the hot path (bare-basename match during
/// directory prune) becomes a single `HashSet::contains` lookup against the dir's basename —
/// no `rel_path` allocation, no per-pattern string ops.
pub struct ExcludeMatcher {
    /// Trailing-slash bare-basename patterns (e.g. `Library/`, `Pods/`). Match anywhere in tree.
    /// Stored without trailing slash.
    pub bare_basenames: HashSet<String>,
    /// Trailing-slash path-containing patterns (e.g. `docs/internal/`). Anchored at root.
    pub anchored: Vec<String>,
    /// Patterns containing `*`, `?`, `[…]`. Matched via `fnmatch`.
    pub globs: Vec<String>,
    /// Plain (no trailing slash, no glob) patterns — match as path prefix at root.
    pub plain_prefixes: Vec<String>,
    /// Trailing-slash glob patterns (e.g. `assets/loc/*/`).
    pub trailing_globs: Vec<(String, usize)>,
}

#[inline]
fn is_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

impl ExcludeMatcher {
    pub fn new<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut bare = HashSet::new();
        let mut anchored = Vec::new();
        let mut globs = Vec::new();
        let mut plain = Vec::new();
        let mut trailing_globs = Vec::new();

        for p in patterns {
            let p = p.into();
            let glob = is_glob(&p);
            if let Some(prefix) = p.strip_suffix('/') {
                if glob {
                    let depth = prefix.split('/').filter(|s| !s.is_empty()).count();
                    trailing_globs.push((prefix.to_string(), depth));
                } else if !prefix.contains('/') {
                    bare.insert(prefix.to_string());
                } else {
                    anchored.push(p);
                }
            } else if glob {
                globs.push(p);
            } else {
                plain.push(p);
            }
        }

        Self {
            bare_basenames: bare,
            anchored,
            globs,
            plain_prefixes: plain,
            trailing_globs,
        }
    }

    /// Fast basename-only check — matches a bare-basename pattern at any depth without
    /// constructing the full rel_path. Returns true if `basename` is in the bare set.
    #[inline]
    pub fn matches_bare(&self, basename: &str) -> bool {
        self.bare_basenames.contains(basename)
    }

    /// Returns true iff the matcher has only bare-basename patterns (no anchored/glob fallback).
    /// When true, callers can skip rel_path allocation entirely.
    #[inline]
    pub fn is_bare_only(&self) -> bool {
        self.anchored.is_empty()
            && self.globs.is_empty()
            && self.plain_prefixes.is_empty()
            && self.trailing_globs.is_empty()
    }

    /// Full rel_path check — used after `matches_bare` returns false, or when the caller already
    /// has rel_path in hand (file-level checks).
    pub fn matches(&self, rel_path: &str, basename: Option<&str>) -> bool {
        if let Some(b) = basename {
            if self.bare_basenames.contains(b) {
                return true;
            }
        }
        if !self.bare_basenames.is_empty() {
            // Fall back to scanning segments — needed when caller can't isolate basename.
            for seg in rel_path.split('/').filter(|s| !s.is_empty()) {
                if self.bare_basenames.contains(seg) {
                    return true;
                }
            }
        }
        for prefix in &self.plain_prefixes {
            if rel_path == prefix
                || rel_path.starts_with(&format!("{}/", prefix))
            {
                return true;
            }
        }
        for pattern in &self.anchored {
            if rel_path.starts_with(pattern) {
                return true;
            }
        }
        for pattern in &self.globs {
            if fnmatch_str(pattern, rel_path) {
                return true;
            }
        }
        for (prefix, depth) in &self.trailing_globs {
            let parts: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() > *depth {
                let dir = parts[..*depth].join("/");
                if fnmatch_str(prefix, &dir) {
                    return true;
                }
            }
        }
        false
    }
}

fn fnmatch_str(pattern: &str, name: &str) -> bool {
    let Ok(p) = CString::new(pattern) else { return false };
    let Ok(n) = CString::new(name) else { return false };
    unsafe { fnmatch(p.as_ptr(), n.as_ptr(), FNM_PATHNAME) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(patterns: &[&str]) -> ExcludeMatcher {
        ExcludeMatcher::new(patterns.iter().map(|s| s.to_string()))
    }

    fn matches(patterns: &[&str], rel_path: &str) -> bool {
        let m = matcher(patterns);
        let basename = rel_path.rsplit('/').next();
        m.matches(rel_path, basename)
    }

    // Bare-basename trailing-slash patterns match at any depth.
    #[test]
    fn bare_basename_at_any_depth() {
        assert!(matches(&["Pods/"], "Pods/Firebase/README.md"));
        assert!(matches(&["Pods/"], "proj-ios/Pods/Firebase/README.md"));
        assert!(matches(&["Pods/"], "a/b/c/Pods/x.md"));
        assert!(matches(&["Pods/"], "a/b/Pods"));
        assert!(!matches(&["Pods/"], "PodsExtra/x.md"));
        assert!(!matches(&["Pods/"], "a/PodsExtra/x.md"));
    }

    // Path-anchored trailing-slash patterns stay root-only.
    #[test]
    fn path_anchored_trailing_slash_stays_root_only() {
        assert!(matches(&["docs/draft/"], "docs/draft/x.md"));
        assert!(!matches(&["docs/draft/"], "a/docs/draft/x.md"));
    }

    #[test]
    fn excludes_exact_prefix() {
        assert!(matches(&["Library"], "Library/foo/bar.md"));
    }

    #[test]
    fn excludes_nested_prefix() {
        // "proj-ios" plain prefix matches all under it
        assert!(matches(&["proj-ios"], "proj-ios/Pods/Firebase/README.md"));
    }

    #[test]
    fn does_not_exclude_partial_match() {
        assert!(!matches(&["Library"], "LibraryExtra/file.md"));
    }

    #[test]
    fn excludes_with_glob() {
        assert!(matches(&["docs/draft-*.md"], "docs/draft-intro.md"));
    }

    #[test]
    fn glob_does_not_match_different_name() {
        assert!(!matches(&["docs/draft-*.md"], "docs/guide.md"));
    }

    #[test]
    fn glob_does_not_match_deeper() {
        // FNM_PATHNAME — `*` doesn't cross `/`
        assert!(!matches(&["docs/draft-*.md"], "docs/sub/draft-intro.md"));
    }

    #[test]
    fn glob_with_question_mark() {
        assert!(matches(&["docs/v?.md"], "docs/v1.md"));
        assert!(!matches(&["docs/v?.md"], "docs/v12.md"));
    }

    #[test]
    fn trailing_slash_as_prefix_plain() {
        assert!(matches(&["assets/localizer/"], "assets/localizer/cat-profiles/00_Normal.md"));
        assert!(!matches(&["assets/localizer/"], "assets/other/file.md"));
    }

    #[test]
    fn trailing_slash_as_prefix_glob() {
        assert!(matches(&["assets/localizer/*/"], "assets/localizer/cat-profiles/00_Normal.md"));
        assert!(matches(&["assets/localizer/*/"], "assets/localizer/prompts/deep/file.md"));
        assert!(!matches(&["assets/localizer/*/"], "assets/localizer/top.md"));
    }

    #[test]
    fn mixes_prefix_and_glob() {
        assert!(matches(&["Library", "docs/draft-*.md"], "Library/foo.md"));
        assert!(matches(&["Library", "docs/draft-*.md"], "docs/draft-intro.md"));
        assert!(!matches(&["Library", "docs/draft-*.md"], "docs/guide.md"));
    }

    #[test]
    fn excludes_file_directly() {
        assert!(matches(&["CHANGELOG.md"], "CHANGELOG.md"));
    }

    #[test]
    fn matches_bare_fast_path() {
        let m = matcher(&["Library/", "Pods/"]);
        assert!(m.matches_bare("Library"));
        assert!(m.matches_bare("Pods"));
        assert!(!m.matches_bare("docs"));
        assert!(m.is_bare_only());
    }

    #[test]
    fn is_bare_only_false_with_anchored() {
        let m = matcher(&["Library/", "docs/internal/"]);
        assert!(!m.is_bare_only());
    }
}
