//! Per-file extraction cache. Mirrors `Sources/Lib/Cache.swift`.
//!
//! Validation key: mtime_ns + size + fnv1a64 content hash. Atomic writes via tempfile.
//!
//! **Schema bumped 2 → 3** for the Rust port. Swift Codable for `Link.Kind` produced
//! `{"crossRepo":{"repo":"…"}}` (externally tagged); serde derive produces a different shape.
//! Bumping invalidates Swift-era caches harmlessly (regenerable).

use crate::config::home_dir;
use crate::extract::{extract_headings, extract_links, Link};
use crate::path::{read_file, rel_path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bumped on any change to on-disk JSON shape OR scanner output. Files written under a
/// different schema are treated as cache miss and silently overwritten.
pub const CACHE_SCHEMA_VERSION: u32 = 3;

/// One cache file per repo at `$XDG_CONFIG_HOME/md-orphan/cache/<fnv1a64-of-canonical-root>.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct LinkCache {
    pub schema_version: u32,
    pub display_name: String,
    pub entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    pub mtime_ns: i64,
    pub size: i64,
    pub content_hash: u64,
    pub links: Vec<Link>,
    pub headings: Vec<String>,
}

impl LinkCache {
    pub fn new(display_name: String) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            display_name,
            entries: HashMap::new(),
        }
    }
}

/// Extracted data for a file: links + headings.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub links: Vec<Link>,
    pub headings: BTreeSet<String>,
}

// MARK: - Cache directory + filename

pub(crate) fn cache_directory() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", home_dir()));
    PathBuf::from(format!("{}/md-orphan/cache", base))
}

pub(crate) fn cache_file_path(canonical_root: &Path) -> PathBuf {
    let s = canonical_root.to_string_lossy();
    cache_directory().join(format!("{}.json", fnv1a64_hex(&s)))
}

// MARK: - fnv1a64

/// FNV-1a 64-bit hash over UTF-8 bytes. Lowercase hex, no leading zeros — matches Swift's
/// `String(h, radix: 16)` byte-for-byte so cache filenames survive across the Rust port.
pub(crate) fn fnv1a64_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in s.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:x}", h)
}

pub(crate) fn fnv1a64(buf: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in buf {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// MARK: - Load / save

pub(crate) fn load_link_cache(canonical_root: &Path, display_name: &str) -> LinkCache {
    let path = cache_file_path(canonical_root);
    let Ok(raw) = fs::read_to_string(&path) else {
        return LinkCache::new(display_name.to_string());
    };
    let Ok(decoded) = serde_json::from_str::<LinkCache>(&raw) else {
        return LinkCache::new(display_name.to_string());
    };
    if decoded.schema_version != CACHE_SCHEMA_VERSION {
        return LinkCache::new(display_name.to_string());
    }
    decoded
}

pub(crate) fn save_link_cache(cache: &LinkCache, canonical_root: &Path) {
    let dir = cache_directory();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("md-orphan: warning: cannot create cache dir {}: {e}", dir.display());
        return;
    }
    let path = cache_file_path(canonical_root);
    let json = match serde_json::to_vec(cache) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("md-orphan: warning: cannot encode cache: {e}");
            return;
        }
    };
    // Atomic write: tempfile in same dir, write, persist (rename).
    match tempfile::NamedTempFile::new_in(&dir) {
        Ok(mut tmp) => {
            if let Err(e) = tmp.write_all(&json) {
                eprintln!("md-orphan: warning: cannot write cache: {e}");
                return;
            }
            if let Err(e) = tmp.persist(&path) {
                eprintln!("md-orphan: warning: cannot persist cache {}: {}", path.display(), e);
            }
        }
        Err(e) => eprintln!("md-orphan: warning: cannot create tmp for cache: {e}"),
    }
}

// MARK: - ExtractionCache

/// Cache-aware reader. Stat → read → hash → cache lookup. On hit: skip extraction.
/// On miss: extract fresh and update the cache. Caller must `save()` at the end of a run.
pub struct ExtractionCache {
    caches: HashMap<PathBuf, LinkCache>,
    dirty: HashSet<PathBuf>,
    pub enabled: bool,
}

impl ExtractionCache {
    pub fn new(enabled: bool) -> Self {
        Self {
            caches: HashMap::new(),
            dirty: HashSet::new(),
            enabled,
        }
    }

    /// Read + extract a file. `repo_root` must be canonical (realpath-ed).
    /// File outside repo_root → caching skipped, always extracts fresh.
    pub fn read(&mut self, file_path: &Path, repo_root: &Path) -> Option<ExtractedFile> {
        let buf = read_file(file_path).ok()?;
        let file_str = file_path.to_string_lossy().to_string();
        let root_str = repo_root.to_string_lossy().to_string();

        if !self.enabled {
            return Some(extract_fresh(&buf));
        }
        let Some(rel_key) = rel_path(&file_str, &root_str).map(|s| s.to_string()) else {
            return Some(extract_fresh(&buf));
        };

        // Stat for mtime + size.
        let meta = match fs::metadata(file_path) {
            Ok(m) => m,
            Err(_) => return Some(extract_fresh(&buf)),
        };
        let mtime_ns = match meta.modified().and_then(|t| {
            t.duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        }) {
            Ok(d) => d.as_nanos() as i64,
            Err(_) => 0,
        };
        let size = meta.len() as i64;
        let hash = fnv1a64(&buf);

        let cache = self.ensure_loaded(repo_root);
        if let Some(entry) = cache.entries.get(&rel_key) {
            if entry.mtime_ns == mtime_ns
                && entry.size == size
                && entry.content_hash == hash
            {
                let headings: BTreeSet<String> = entry.headings.iter().cloned().collect();
                return Some(ExtractedFile {
                    links: entry.links.clone(),
                    headings,
                });
            }
        }

        let result = extract_fresh(&buf);
        let mut headings_vec: Vec<String> = result.headings.iter().cloned().collect();
        headings_vec.sort();
        let new_entry = CacheEntry {
            mtime_ns,
            size,
            content_hash: hash,
            links: result.links.clone(),
            headings: headings_vec,
        };
        self.caches
            .get_mut(repo_root)
            .expect("ensure_loaded populated this")
            .entries
            .insert(rel_key, new_entry);
        self.dirty.insert(repo_root.to_path_buf());
        Some(result)
    }

    /// Drop entries for files no longer under the repo (auto-prune).
    pub fn prune(&mut self, canonical_root: &Path, keep_relative_paths: &HashSet<String>) {
        let Some(cache) = self.caches.get_mut(canonical_root) else {
            return;
        };
        let removed: Vec<String> = cache
            .entries
            .keys()
            .filter(|k| !keep_relative_paths.contains(k.as_str()))
            .cloned()
            .collect();
        if removed.is_empty() {
            return;
        }
        for k in removed {
            cache.entries.remove(&k);
        }
        self.dirty.insert(canonical_root.to_path_buf());
    }

    pub fn save(&mut self) {
        if !self.enabled {
            return;
        }
        for root in self.dirty.drain().collect::<Vec<_>>() {
            if let Some(cache) = self.caches.get(&root) {
                save_link_cache(cache, &root);
            }
        }
    }

    fn ensure_loaded(&mut self, canonical_root: &Path) -> &mut LinkCache {
        if !self.caches.contains_key(canonical_root) {
            let display_name = canonical_root
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let cache = load_link_cache(canonical_root, &display_name);
            self.caches.insert(canonical_root.to_path_buf(), cache);
        }
        self.caches.get_mut(canonical_root).unwrap()
    }
}

fn extract_fresh(buf: &[u8]) -> ExtractedFile {
    ExtractedFile {
        links: extract_links(buf),
        headings: extract_headings(buf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::LinkKind;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn fnv1a64_stability() {
        assert_eq!(fnv1a64_hex("hello"), fnv1a64_hex("hello"));
        assert_ne!(fnv1a64_hex("hello"), fnv1a64_hex("hellp"));
    }

    #[test]
    fn fnv1a64_known_value() {
        // FNV-1a("hello") canonical value (test vector).
        // Verifies the algorithm matches Swift's identical hand-rolled impl byte-for-byte.
        let h = format!("{:x}", fnv1a64(b"hello"));
        assert_eq!(h, "a430d84680aabd0b");
    }

    #[test]
    fn cache_file_path_hashes_canonical_root() {
        let p1 = cache_file_path(Path::new("/Users/jk/proj-a"));
        let p2 = cache_file_path(Path::new("/Users/jk/proj-b"));
        assert_ne!(p1, p2);
        assert!(p1.to_string_lossy().ends_with(".json"));
        assert!(p1.to_string_lossy().contains("/cache/"));
    }

    #[test]
    fn cache_round_trips_via_disk() {
        let tmp = TempDir::new().unwrap();
        // Override XDG_CONFIG_HOME so save_link_cache writes inside our tmp.
        // SAFETY: cargo test is parallel by default but env is process-global. We use
        // a unique path per-test to avoid clobbering, and rely on the fact that other
        // tests don't touch XDG_CONFIG_HOME.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()); }

        // Use canonical path: macOS /var/folders → /private/var/folders.
        let canon = fs::canonicalize(tmp.path()).unwrap();

        let mut cache = LinkCache::new("round-trip".into());
        cache.entries.insert(
            "index.md".into(),
            CacheEntry {
                mtime_ns: 1_700_000_000_000_000_000,
                size: 42,
                content_hash: 0xdeadbeef,
                links: vec![
                    Link {
                        kind: LinkKind::Wiki,
                        target: "foo.md".into(),
                        fragment: None,
                        path_start: 5,
                        path_end: 11,
                    },
                    Link {
                        kind: LinkKind::Standard,
                        target: "bar.md".into(),
                        fragment: Some("sec".into()),
                        path_start: 20,
                        path_end: 26,
                    },
                    Link {
                        kind: LinkKind::CrossRepo { repo: "r".into() },
                        target: "baz.md".into(),
                        fragment: None,
                        path_start: 30,
                        path_end: 36,
                    },
                ],
                headings: vec!["intro".into(), "details".into()],
            },
        );

        save_link_cache(&cache, &canon);
        let loaded = load_link_cache(&canon, "round-trip");

        assert_eq!(loaded.schema_version, CACHE_SCHEMA_VERSION);
        assert_eq!(loaded.entries.len(), 1);
        let entry = loaded.entries.get("index.md").unwrap();
        assert_eq!(entry.content_hash, 0xdeadbeef);
        assert_eq!(entry.links.len(), 3);
        assert_eq!(entry.links[0].kind, LinkKind::Wiki);
        assert_eq!(entry.links[1].kind, LinkKind::Standard);
        assert_eq!(entry.links[2].kind, LinkKind::CrossRepo { repo: "r".into() });
        assert_eq!(entry.links[1].fragment, Some("sec".into()));

        unsafe { std::env::remove_var("XDG_CONFIG_HOME"); }
    }

    #[test]
    fn extraction_cache_invalidates_on_content_change() {
        let dir = TempDir::new().unwrap();
        let canonical_dir = fs::canonicalize(dir.path()).unwrap();
        let path = canonical_dir.join("index.md");
        fs::write(&path, "[[a.md]]").unwrap();

        let mut cache = ExtractionCache::new(true);
        cache.read(&path, &canonical_dir).unwrap();

        // Sleep briefly so mtime differs even on coarse FS, then change content + size.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "[[b.md]] and [[c.md]]").unwrap();

        let result = cache.read(&path, &canonical_dir).unwrap();
        let targets: Vec<String> = result.links.iter().map(|l| l.target.clone()).collect();
        assert_eq!(targets, vec!["b.md", "c.md"]);
    }
}
