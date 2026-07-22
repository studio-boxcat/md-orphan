//! Per-file extraction cache. Validation key: mtime_ns + size + fnv1a64 content hash.
//!
//! See [[architecture.md#persisted-state]] for schema. Atomic writes via [`atomic_write_json`],
//! shared with the walk-cache in [[walk_cache.rs]].

use crate::config::xdg_config_home;
use crate::extract::{extract_headings, extract_links, Link};
use crate::path::{atomic_write_bytes, rel_path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bumped on any change to on-disk JSON shape OR scanner output. Files written under a
/// different schema are treated as cache miss and silently overwritten.
///
/// 4: added `repo_set_hash` so cross-repo backtick refs are filtered against the active
///    repo set; cached `LinkKind::CrossRepo` entries from v3 may have ghost repos.
/// 5: scanner emits `LinkKind::Inline` for plain backtick spans; v4 entries lack them.
/// 6: inline-span guards stop at `#` — raw-text fragments with spaces now emitted.
/// 7: `LinkKind::Inline` removed — plain backtick spans are no longer links; v5/v6 entries
///    contain `inline`-kind links that no longer deserialize.
/// 8: dropped write-only `display_name`.
pub const CACHE_SCHEMA_VERSION: u32 = 8;

/// fnv1a64 of the sorted configured repo-name set. Branded so it can't be cross-passed with
/// the walk-cache's `FlagsKey` — on disk both are bare fnv1a64 u64s (`serde(transparent)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoSetHash(u64);

/// fnv1a64 of a file's bytes — the strongest leg of the per-entry validation key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(u64);

/// One cache file per repo at `$XDG_CONFIG_HOME/md-orphan/cache/<fnv1a64-of-canonical-root>.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct LinkCache {
    pub schema_version: u32,
    /// Mismatch invalidates ALL entries because cross-repo backtick refs are
    /// repo-set-dependent (see [[extract.rs]]).
    pub repo_set_hash: RepoSetHash,
    pub entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    pub mtime_ns: i64,
    pub size: i64,
    pub content_hash: ContentHash,
    pub links: Vec<Link>,
    pub headings: Vec<String>,
}

impl LinkCache {
    pub fn new(repo_set_hash: RepoSetHash) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            repo_set_hash,
            entries: HashMap::new(),
        }
    }
}

/// Stable hash of a sorted joined repo-name list. Empty set → 0 sentinel.
pub(crate) fn compute_repo_set_hash(repos: &HashSet<String>) -> RepoSetHash {
    if repos.is_empty() {
        return RepoSetHash(0);
    }
    RepoSetHash(fnv1a64_of_sorted(repos.iter().map(String::as_str)))
}

/// fnv1a64 over a sorted-then-joined-with-NUL list. Stable across input ordering.
pub(crate) fn fnv1a64_of_sorted<'a, I: IntoIterator<Item = &'a str>>(items: I) -> u64 {
    let mut sorted: Vec<&str> = items.into_iter().collect();
    sorted.sort();
    fnv1a64(sorted.join("\0").as_bytes())
}

/// Extracted data for a file: links + headings.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub links: Vec<Link>,
    pub headings: BTreeSet<String>,
}

// MARK: - Cache directory + filename

pub(crate) fn cache_directory() -> PathBuf {
    PathBuf::from(format!("{}/md-orphan/cache", xdg_config_home()))
}

pub(crate) fn cache_file_path(canonical_root: &Path) -> PathBuf {
    let s = canonical_root.to_string_lossy();
    cache_directory().join(format!("{}.json", fnv1a64_hex(&s)))
}

// MARK: - fnv1a64

/// FNV-1a 64-bit hash over UTF-8 bytes. Lowercase hex, no leading zeros — matches Swift's
/// `String(h, radix: 16)` byte-for-byte so cache filenames survive across the Rust port.
pub(crate) fn fnv1a64_hex(s: &str) -> String {
    format!("{:x}", fnv1a64(s.as_bytes()))
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

pub(crate) fn load_link_cache(canonical_root: &Path, repo_set_hash: RepoSetHash) -> LinkCache {
    let fresh = || LinkCache::new(repo_set_hash);
    let path = cache_file_path(canonical_root);
    let Ok(raw) = fs::read_to_string(&path) else { return fresh(); };
    let Ok(decoded) = serde_json::from_str::<LinkCache>(&raw) else { return fresh(); };
    if decoded.schema_version != CACHE_SCHEMA_VERSION || decoded.repo_set_hash != repo_set_hash {
        return fresh();
    }
    decoded
}

pub(crate) fn save_link_cache(cache: &LinkCache, canonical_root: &Path) {
    atomic_write_json(&cache_file_path(canonical_root), cache, "cache");
}

/// Atomic JSON write via [`atomic_write_bytes`]. Failures log to stderr but don't abort —
/// caches are regenerable, so a write error means a cache miss next run.
pub(crate) fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T, label: &str) {
    let result = serde_json::to_vec(value)
        .map_err(std::io::Error::other)
        .and_then(|json| atomic_write_bytes(path, &json));
    if let Err(e) = result {
        eprintln!("md-orphan: warning: cannot write {label} {}: {e}", path.display());
    }
}

// MARK: - ExtractionCache

/// Cache-aware reader. Stat → read → hash → cache lookup. On hit: skip extraction.
/// On miss: extract fresh and update the cache. Caller must `save()` at the end of a run.
pub struct ExtractionCache {
    caches: HashMap<PathBuf, LinkCache>,
    dirty: HashSet<PathBuf>,
    pub enabled: bool,
    /// See [[extract.rs]] for the cross-repo annotation filter.
    repos: HashSet<String>,
    repo_set_hash: RepoSetHash,
}

impl ExtractionCache {
    pub fn new(enabled: bool, repos: HashSet<String>) -> Self {
        let repo_set_hash = compute_repo_set_hash(&repos);
        Self {
            caches: HashMap::new(),
            dirty: HashSet::new(),
            enabled,
            repos,
            repo_set_hash,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(enabled: bool) -> Self {
        Self::new(enabled, HashSet::new())
    }

    /// Read + extract a file. `repo_root` must be canonical (realpath-ed).
    /// File outside repo_root → caching skipped, always extracts fresh.
    pub fn read(&mut self, file_path: &Path, repo_root: &Path) -> Option<ExtractedFile> {
        let buf = fs::read(file_path).ok()?;
        let file_str = file_path.to_string_lossy().to_string();
        let root_str = repo_root.to_string_lossy().to_string();

        if !self.enabled {
            return Some(extract_fresh(&buf, &self.repos));
        }
        let Some(rel_key) = rel_path(&file_str, &root_str).map(|s| s.to_string()) else {
            return Some(extract_fresh(&buf, &self.repos));
        };

        let meta = match fs::metadata(file_path) {
            Ok(m) => m,
            Err(_) => return Some(extract_fresh(&buf, &self.repos)),
        };
        let mtime_ns = mtime_ns_or_zero(&meta);
        let size = meta.len() as i64;
        let hash = ContentHash(fnv1a64(&buf));

        let cache = self.ensure_loaded(repo_root);
        if let Some(entry) = cache.entries.get(&rel_key)
            && entry.mtime_ns == mtime_ns
                && entry.size == size
                && entry.content_hash == hash
            {
                let headings: BTreeSet<String> = entry.headings.iter().cloned().collect();
                return Some(ExtractedFile {
                    links: entry.links.clone(),
                    headings,
                });
            }

        let result = extract_fresh(&buf, &self.repos);
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
            let cache = load_link_cache(canonical_root, self.repo_set_hash);
            self.caches.insert(canonical_root.to_path_buf(), cache);
        }
        self.caches.get_mut(canonical_root).unwrap()
    }
}

/// mtime in ns, or 0 when the stat/clock is unusable. Deliberate degrade, not a skip: the
/// entry's `content_hash` (+ size) still gates hits, so a zero mtime can never produce a
/// stale hit — it only forces the hash comparison every run.
fn mtime_ns_or_zero(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn extract_fresh(buf: &[u8], repos: &HashSet<String>) -> ExtractedFile {
    ExtractedFile {
        links: extract_links(buf, repos),
        headings: extract_headings(buf),
    }
}

/// Process-global lock to serialize tests that mutate `XDG_CONFIG_HOME`. Shared with
/// `walk_cache::tests` since the env is per-process and both modules write under XDG.
#[cfg(test)]
pub(crate) fn xdg_test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|p| p.into_inner())
}

/// Isolated XDG dir + sibling repo dir under one tempdir, so cache writes don't bump the
/// indexed repo's mtimes. Returns `(tempdir guard, xdg dir, canonicalized repo dir)`.
/// Shared by `discovery`/`walk_cache` tests; caller must hold [`xdg_test_lock`].
#[cfg(test)]
pub(crate) fn xdg_test_env() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let xdg = tmp.path().join("xdg");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&xdg).unwrap();
    fs::create_dir_all(&repo).unwrap();
    // SAFETY: caller holds `xdg_test_lock()`; env is process-global but serialized.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &xdg) };
    let canon = fs::canonicalize(&repo).unwrap();
    (tmp, xdg, canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::LinkKind;
    use std::fs;
    use std::path::Path;

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
        let _g = xdg_test_lock();
        let (_tmp, _xdg, canon) = xdg_test_env();

        let mut cache = LinkCache::new(RepoSetHash(0));
        cache.entries.insert(
            "index.md".into(),
            CacheEntry {
                mtime_ns: 1_700_000_000_000_000_000,
                size: 42,
                content_hash: ContentHash(0xdeadbeef),
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
        let loaded = load_link_cache(&canon, RepoSetHash(0));

        assert_eq!(loaded.schema_version, CACHE_SCHEMA_VERSION);
        assert_eq!(loaded.entries.len(), 1);
        let entry = loaded.entries.get("index.md").unwrap();
        assert_eq!(entry.content_hash, ContentHash(0xdeadbeef));
        assert_eq!(entry.links.len(), 3);
        assert_eq!(entry.links[0].kind, LinkKind::Wiki);
        assert_eq!(entry.links[1].kind, LinkKind::Standard);
        assert_eq!(entry.links[2].kind, LinkKind::CrossRepo { repo: "r".into() });
        assert_eq!(entry.links[1].fragment, Some("sec".into()));
    }

    #[test]
    fn extraction_cache_invalidates_on_content_change() {
        let _g = xdg_test_lock();
        // xdg_test_env redirects XDG so ExtractionCache::read doesn't pollute the user's
        // real cache dir.
        let (_tmp, _xdg, canonical_dir) = xdg_test_env();
        let path = canonical_dir.join("index.md");
        fs::write(&path, "[[a.md]]").unwrap();

        let mut cache = ExtractionCache::for_tests(true);
        cache.read(&path, &canonical_dir).unwrap();

        // Sleep briefly so mtime differs even on coarse FS, then change content + size.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, "[[b.md]] and [[c.md]]").unwrap();

        let result = cache.read(&path, &canonical_dir).unwrap();
        let targets: Vec<String> = result.links.iter().map(|l| l.target.clone()).collect();
        assert_eq!(targets, vec!["b.md", "c.md"]);
    }

    #[test]
    fn extraction_cache_invalidates_on_repo_set_change() {
        let _g = xdg_test_lock();
        let (_tmp, _xdg, canon) = xdg_test_env();
        let path = canon.join("index.md");
        // `foo.md` (some-repo) — cross-repo ref to "some-repo".
        fs::write(&path, "see `foo.md` (some-repo)").unwrap();

        // First run with "some-repo" configured: parser emits CrossRepo.
        let repos_a: HashSet<String> = ["some-repo".to_string()].into_iter().collect();
        let mut cache_a = ExtractionCache::new(true, repos_a);
        let first = cache_a.read(&path, &canon).unwrap();
        assert_eq!(first.links.len(), 1);
        cache_a.save();

        // Second run with empty repo set: cached entry has different repo_set_hash → load
        // returns a fresh empty cache → re-extract with empty set → the ref demotes from
        // CrossRepo to plain inline code (no link at all).
        let mut cache_b = ExtractionCache::new(true, HashSet::new());
        let second = cache_b.read(&path, &canon).unwrap();
        assert!(second.links.is_empty(), "expected re-extract with no links, got {:?}", second.links);
    }
}
