# TODO

## Deferred follow-ups

### Possible: extend parallel discovery to transitive cross-repos
The current prefetch only catches first-level cross-repo refs in the seeded entry files. Cross-repo targets discovered deeper (e.g. meow-tower's `docs/specs/*` referencing `meow-game-server`) fall back to lazy serial walks. Could switch BFS to level-synchronous and parallel-batch all cross-repo refs at each level boundary. Win on meow-tower-like setups: ~100-200 ms (sum of ~3 transitive cross-repo walks → max). Trade-off: FIFO crawl order becomes level-order. Defer until profiling shows the lazy serial fallback dominating.

### Concurrent invocation safety
Cache writes are last-writer-wins. If users start running two `md-orphan` invocations against the same repo simultaneously (e.g. editor save hooks + lefthook), consider `flock(2)` on the cache file. So far accepted as last-writer-wins because cache is regenerable.

### XDG nit
`expand_path` understands `$VAR` and leading `~/`. It does not currently expand `~user/` (other-user homes). Nobody asked, but worth mentioning.

### Multiple entry points spanning sibling dirs
`main.rs` does `let root = dir_name(&resolved_entries[0])` — if the user passes `a/index.md b/index.md` where `a/` and `b/` are siblings, the second entry is treated as living under `a/`'s root (broken-link-prone). Either (a) reject multi-entry across sibling dirs, or (b) compute a common ancestor. Pre-existing edge case; not a regression.

### Walk-cache: XDG inside indexed root
If `XDG_CONFIG_HOME` resolves under the entry repo (unusual — e.g. running md-orphan over `$HOME` with default `~/.config`), saves bump dir mtimes that the walk recorded, causing guaranteed misses on the next run. The dot-dir prune handles `~/.config`, but a custom non-default XDG inside the tree would force cold-every-run. Either: refuse to persist when `walk_cache_directory()` is under `canonical_root`, or document the constraint. Low priority; affects only users with non-standard XDG layouts.

### Walk-cache: non-UTF-8 path lossy comparison
`walk_cache.rs` compares `cache.canonical_root` against `canonical_root.to_string_lossy()` (`try_load_walk_cache`) and hashes via the lossy form for the cache filename. Two distinct paths with non-UTF-8 bytes could lossy-collide, returning a wrong-repo cache. Not a hazard for current users (macOS APFS is UTF-8; Linux dev paths typically are too). Fix would be to hash raw `OsStr` bytes via `as_encoded_bytes()` and reject non-UTF-8 paths from the equality check, or store both. Defer.

### Walk-cache: NFS / coarse-mtime filesystems
Validation assumes dir mtime is bumped reliably on entry add/remove/rename. APFS, ext4, btrfs, NTFS: yes. NFS and some FUSE/CIFS mounts: server-side mtime can lag or have second-granularity. A rapid add+walk+save cycle on NFS could record a stale mtime that survives validation. No clean fix without entry-content-hashing per dir (expensive). Document the hazard if anyone reports it.

### Trim post-walk pipeline (~30 ms BFS + cross-repo crawl)
After walk-cache, the warm-path floor is the BFS-resolve + style-check + cross-repo coordination block in `crawl.rs`. Burns ~25-30 ms on meow-tower. No quick win identified; would require profiling to see whether path resolution, basename lookups, or cross-repo dispatch dominates. The walk has been eliminated from steady-state; this is now the largest remaining lever for warm-path speedup.

### Considered cache-shape alternatives (rejected, recorded for future revisit)
Asked during this session: what about pre-built Rust cache crates / faster persistence formats? Conclusions:
- **`cacache` / `redb` / `sled`** — too heavy. Their startup cost (open log, replay, schema check) often exceeds md-orphan's entire warm-path budget. They also don't help with the mtime-validated invalidation, which is the actual hard part.
- **`atomicwrites` crate** — a real swap target. Could replace `cache::atomic_write_json` with a 5-LOC call, but the dedup is already done so the win is just reducing local code.
- **`rkyv` / `bincode` zero-copy load** — would shave ~1 ms off JSON parse on warm. The bottleneck isn't parse though; it's the ~1k `stat()` syscalls for dir validation.
- **Merkle-style root-mtime hash (1 stat instead of 1k)** — would cut validation from ~3 ms to ~50 µs. Tradeoff: invalidates aggressively (any dir change in the tree → full re-walk). Worth revisiting if warm-path budget tightens.
- **`rayon`-parallel validation stats** — could cut ~3 ms to ~0.5 ms on 6 cores, but rayon's thread-pool spinup eats most of the win for a one-shot CLI.

Reason for rejecting all: warm path is ~40 ms, cache load is ~3 ms of that. Even cutting cache load to zero only takes ~37 ms. The 30 ms BFS+crawl above is the actual ceiling. Defer until that's addressed.

