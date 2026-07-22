//! Path helpers: real_path, dir_name, base_name, rel_path, atomic_write_bytes.
//!
//! Mirrors `Sources/Lib/Util.swift`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve symlinks + return canonical absolute path. Returns `None` on missing path.
/// Equivalent to `realpath(3)`. Empty/garbage paths return `None` (Swift's behavior).
pub fn real_path<P: AsRef<Path>>(path: P) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
}

/// Symlink-resolved absolute path — `realpath(3)` output. The crawl's identity key: visited
/// sets, the heading cache, and reachability are all keyed by this. The brand exists because
/// mixing raw and canonical paths breaks `starts_with` containment checks and visited-set
/// dedup (macOS `/var/folders` ↔ `/private/var/folders` — architecture.md, Pitfalls).
///
/// Construct via [`canonicalize_str`]; `assume_canonical` is the documented escape hatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalPath(String);

impl CanonicalPath {
    /// Brand a path that can't go through `realpath(3)` — e.g. a seeded entry file that
    /// doesn't exist (surfaces as `Unreadable` at visit). Callers own the canonicality claim.
    pub(crate) fn assume_canonical(path: String) -> Self {
        Self(path)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CanonicalPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<Path> for CanonicalPath {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

/// Lets `HashSet<CanonicalPath>`/`HashMap<CanonicalPath, _>` look up by `&str` without
/// re-branding the probe (derived `Hash` delegates to the inner `String` = `str` hashing).
impl std::borrow::Borrow<str> for CanonicalPath {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// [`real_path`] + lossy decode into the crawl's `String`-keyed canonical form.
pub fn canonicalize_str<P: AsRef<Path>>(path: P) -> Option<CanonicalPath> {
    real_path(path).map(|p| CanonicalPath(p.to_string_lossy().into_owned()))
}

/// Parent directory portion of `path`. Returns `"."` when there's no `/`.
/// Mirrors Swift `dirName`: lastIndex of `/`, take prefix; else `"."`.
pub fn dir_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[..idx],
        None => ".",
    }
}

/// Final path component. Returns the input unchanged when there's no `/`.
/// Mirrors Swift `baseName`.
pub fn base_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}

/// Strip `root + "/"` prefix from `path` when present. Returns `Some("")` when `path == root`.
/// Returns `None` when `path` is outside `root`.
pub fn rel_path<'a>(path: &'a str, under_root: &str) -> Option<&'a str> {
    if path == under_root {
        return Some("");
    }
    let rest = path.strip_prefix(under_root)?;
    rest.strip_prefix('/')
}

/// Containment check: `path == root` or `path` lives under `root/`. Allocation-free —
/// replaces the `x == root || x.starts_with(&format!("{root}/"))` idiom.
pub fn is_under(path: &str, root: &str) -> bool {
    rel_path(path, root).is_some()
}

/// Tempfile + rename atomic write (tempfile in the target's own dir so rename stays on-device).
/// Callers decide failure policy: caches warn and continue (regenerable), `--fix` fails the run.
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.persist(path)?;
    Ok(())
}

/// Test helper: write `content` to `path`, creating parent dirs. Shared by crawl/discovery tests.
#[cfg(test)]
pub(crate) fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

// MARK: - Byte-scanner helpers (internal, shared with extract.rs)

/// Check if a path segment has a file extension (a `.` followed by 1+ chars, not preceded by `/`).
#[inline]
pub(crate) fn has_extension(bytes: &[u8], from: usize, len: usize) -> bool {
    if len < 3 {
        return false; // minimum: "a.b"
    }
    let mut i = from + len - 1;
    while i > from {
        let b = bytes[i];
        if b == b'.' {
            return true;
        }
        if b == b'/' {
            return false;
        }
        i -= 1;
    }
    false
}

#[inline]
pub(crate) fn decode_utf8(bytes: &[u8], from: usize, len: usize) -> String {
    String::from_utf8_lossy(&bytes[from..from + len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_name_of_file_path() {
        assert_eq!(dir_name("/repo/docs/file.md"), "/repo/docs");
    }

    #[test]
    fn dir_name_of_root_file() {
        assert_eq!(dir_name("/file.md"), "");
    }

    #[test]
    fn dir_name_no_slash() {
        assert_eq!(dir_name("file.md"), ".");
    }

    #[test]
    fn base_name_extracts_final_segment() {
        assert_eq!(base_name("/repo/docs/file.md"), "file.md");
    }

    #[test]
    fn base_name_no_slash() {
        assert_eq!(base_name("file.md"), "file.md");
    }

    #[test]
    fn rel_path_strips_root_prefix() {
        assert_eq!(rel_path("/repo/docs/file.md", "/repo"), Some("docs/file.md"));
    }

    #[test]
    fn rel_path_equals_root_returns_empty() {
        assert_eq!(rel_path("/repo", "/repo"), Some(""));
    }

    #[test]
    fn rel_path_outside_root_returns_none() {
        assert_eq!(rel_path("/other/file.md", "/repo"), None);
    }

    #[test]
    fn has_extension_simple() {
        let s = b"foo.md";
        assert!(has_extension(s, 0, s.len()));
    }

    #[test]
    fn has_extension_no_dot() {
        let s = b"foo";
        assert!(!has_extension(s, 0, s.len()));
    }

    #[test]
    fn has_extension_leading_dot_not_an_extension() {
        // A dot at the very start of the segment (`.md`) isn't an extension — there's no
        // basename char before it. The scan stops at `from` without inspecting position 0.
        assert!(!has_extension(b".md", 0, 3));
    }

    #[test]
    fn has_extension_short_string() {
        assert!(!has_extension(b"a", 0, 1));
        assert!(!has_extension(b"ab", 0, 2));
    }

    #[test]
    fn decode_utf8_basic() {
        assert_eq!(decode_utf8(b"hello", 0, 5), "hello");
    }

    #[test]
    fn decode_utf8_slice() {
        assert_eq!(decode_utf8(b"hello world", 6, 5), "world");
    }
}
