//! Path helpers: real_path, dir_name, base_name, rel_path, read_file.
//!
//! Mirrors `Sources/Lib/Util.swift`. Swift had a global reusable read buffer; Rust uses per-call
//! `Vec<u8>` (allocator is fast enough at our file sizes — sub-ms for ~10KB files).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve symlinks + return canonical absolute path. Returns `None` on missing path.
/// Equivalent to `realpath(3)`. Empty/garbage paths return `None` (Swift's behavior).
pub fn real_path<P: AsRef<Path>>(path: P) -> Option<PathBuf> {
    fs::canonicalize(path).ok()
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
    let prefix = format!("{}/", under_root);
    if let Some(rest) = path.strip_prefix(&prefix) {
        Some(rest)
    } else {
        None
    }
}

/// Read file contents as raw bytes. Per-call `Vec<u8>` allocation; no shared global buffer.
/// Tests can run in parallel because of this (vs. Swift's `@Suite(.serialized)` constraint).
pub fn read_file<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    fs::read(path)
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
