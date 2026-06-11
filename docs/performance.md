> **Related:** [[CLAUDE.md]], [[architecture.md]]

# Performance

Wall-clock numbers, where time goes, and what the cache and `--no-default-excludes` flags actually buy you. Re-measured after Swift→Rust migration; numbers improved ~30% over the Swift baseline.

## Test bench

- macOS arm64 (M-series), APFS local volume, release build (`cargo build --release`)
- 5–7 runs each, post-warmup
- **Self-check**: this repo, ~6 `.md` files, entry `CLAUDE.md`
- **Unity-scale**: meow-tower repo, ~51k files (Library/Pods/proj-*/Packages excluded by defaults + `.md-orphan`), 116 reachable `.md` files

## Wall clock (Rust binary, parallel walker via `ignore::WalkParallel`)

| Scenario | min – median – max | CPU | Notes |
|---|---|---|---|
| Self-check (6 `.md`), warm | **4 – 6 – 9 ms** | ~75% | startup + thread pool spinup dominates |
| Unity-scale, `--no-cache` | **59 – 63 – 75 ms** | ~450-650% | walk + read + extract; no cross-repo recursion |
| Unity-scale, **warm walk-cache + per-file** | **26 – 29 – 38 ms** | ~96% | no walk, no thread pool — single-threaded happy path |
| Unity-scale, `--no-default-excludes` | ~700 ms | ~500% | walks Library/Pods/etc. — parallelism saves more on bigger trees |

Progression on meow-tower:

| Stage | Time | Δ |
|---|---|---|
| Swift baseline (pre-Rust) | ~225 ms | — |
| Rust port (sequential `walkdir`) | ~165 ms | -27% |
| + drop `sort_by_file_name` + precompute root prefix | ~165 ms (best 164 ms) | wash on wall clock; -30 ms user time |
| + `ignore::WalkParallel` (5 worker threads) | ~99 ms (best 91 ms) | -40% |
| + walk-cache (warm) | ~40 ms (best 38 ms) | -60% |
| + drop cross-repo recursion (warm) | **~29 ms (best 26 ms)** | **-28%** |

**Total Swift→warm-today: 225 ms → 29 ms (-87%).**

| Tool | meow-tower (51k pruned) |
|---|---|
| `fd ''` (parallel, walk only) | 33 – 35 ms |
| `fd '' --threads 1` (walk only) | 180 – 210 ms |
| md-orphan, cold (`--no-cache`) | 59 – 63 ms |
| md-orphan, warm | **26 – 29 ms** |

Warm md-orphan is in the same ballpark as `fd ''` parallel — but `fd` only lists files, while md-orphan also reads 116 `.md` files, extracts links/headings, BFS-resolves, style-checks, and crawls 3 cross-repo targets.

## Where time goes (warm path, ~29 ms total)

- Walk-cache load + `dir_mtimes` validation stats — ~3 ms. ~1k surviving dirs after prune; sequential `stat()` per dir but no `getdents` traversal.
- Per-file cache validation on entry-repo `.md` files (read + hash) — ~5 ms across 116 files.
- BFS resolve + style check + cross-repo direct-target reads (no recursion) — ~15-18 ms.
- Output rendering + process exit — ~3-5 ms.

The walk has been **eliminated from steady-state runs**. What remains is the post-walk pipeline: per-file cache validation reads, BFS resolution, and cross-repo crawl coordination.

## Where time goes (cold path, ~63 ms `--no-cache`)

- `index_repo` via `ignore::WalkParallel` — ~35 ms. Bound by kernel `getdents` traversal of ~51k entries, parallelized across 5 worker threads.
- Read + extract 116 entry-repo `.md` files — ~5 ms. Manual byte scanner; ~0.04 ms/file.
- BFS resolve + style check + cross-repo direct-target reads — ~15-20 ms. No recursion into cross-repo files.
- Output rendering + process exit — ~3-5 ms.

## Why the walk-cache earns its keep here

The walk-cache (`walk_cache.rs`) persists `RepoIndex` keyed by canonical root + flags hash, validated by per-dir mtime. APFS bumps a directory's mtime on entry add/remove/rename — not on file content edits — which is exactly the granularity we need for "did the basename map change?". File content edits are caught by the per-file cache layer below.

- **Cold first run** pays ~36 ms over `--no-cache`: per-dir `stat()` calls during the walk + cache write. Acceptable one-time tax.
- **Warm runs** drop ~60 ms: zero `getdents`, zero parallel walker spinup, just a stat-per-dir validation pass before the BFS pipeline runs.
- Schema-versioned + `flags_key`-keyed: changes to `--exclude`, `.md-orphan`, or `--no-default-excludes` invalidate immediately.

Disable with `--no-cache` (toggles both walk-cache and per-file extraction cache).

## Why the per-file cache is break-even here

The per-file extraction cache (`cache.rs`) exists, is correct (mtime + size + fnv1a64 content hash, atomic writes, schema-versioned), and shaves the ~5 ms extraction step on subsequent runs. But:

- The validation path **always reads the file and hashes it** — that's the bulk of what a fresh extraction costs.
- Hashing 600 KB of `.md` content is microseconds; extraction itself is also microseconds.
- Net wall-clock difference between cold-no-cache and warm-with-per-file (without walk-cache) is within run-to-run variance (~5 ms).

Where it still earns its keep:

- Larger doc trees (1000s of `.md` files) — extraction starts to add up.
- Per-file content hashes catch the "post-`--fix` re-run with byte-equal output" scenario where `mtime + size` could be unchanged but cached byte offsets are stale.

The walk-cache is the larger lever here, since it eliminates the ~35 ms walk entirely rather than shaving ~5 ms off extraction.

## Parallel cross-repo discovery

When the entry files reference cross-repo targets directly, those repos are indexed in parallel via `std::thread::scope`. Wall-clock cost for N **first-level** referenced repos is `max(walk_time)` instead of `sum(walk_time)`.

The prefetch reads the seeded entry files in `CrawlState.seed`, extracts cross-repo names, and dispatches `index_repo` for those targets in parallel. **Transitively-discovered cross-repos** (refs from a cross-repo file rather than the entry, or from same-repo files reachable from the entry) fall back to lazy serial via `index_for`.

For projects with no cross-repo refs anywhere, prescan finds zero targets and no parallel work runs.

Tracked in [[TODO.md]]: extending to transitive cross-repos via level-synchronous BFS could save more on meow-tower-like setups, at the cost of trading FIFO crawl order for level-by-level batching.

## What dominates

1. **`ignore::WalkParallel` traversal** — kernel-side dirent reads parallelized across N worker threads via crossbeam-deque work stealing. Default excludes prune big subtrees (`Library/`, `Pods/`, etc.); per-repo `.md-orphan` adds project-specific prunes.
2. **`ExcludeMatcher` bare-basename hash lookup** — O(1) per dir entry. `Mutex<HashMap>` insertions for `by_name` and `Mutex<HashSet>` for `md_files` add small contention; not yet measured as a bottleneck.

## What doesn't help further

- **More threads beyond CPU count**: `ignore` defaults to `num_cpus`; pinning to higher values just thrashes context switches.
- **Dropping `Mutex` for thread-local accumulators + merge**: the Mutex-locked sections are microseconds; not the bottleneck.
- **`getattrlistbulk`** (macOS-specific batched stat): known kernel bugs ([Apple Forums](https://developer.apple.com/forums/thread/656787)).
- **regex**: a manual byte scanner is faster than `regex` for our tight scanner pattern (single-char-class transitions).
- **Sequential walk for tiny repos**: thread pool spin-up adds ~5 ms vs. sequential, regressing the 6-file self-check from 3 ms → 7 ms. Could add a sequential fast path under a file-count threshold; not worth it given the absolute cost.

## Reproducing

CLI wall clock (best-of-3):

```
cargo build --release
for i in 1 2 3; do (time target/release/md-orphan --no-cache /path/to/repo/CLAUDE.md > /dev/null) 2>&1 | head -1; done
```

Tests:

```
cargo test                      # unit tests
cargo test --release            # release build
```

## Tracked perf follow-ups

See [[TODO.md]] for:

- Extending parallel discovery to transitive cross-repos (level-synchronous BFS)
- Non-`.md` style support cost analysis (~10× current `index_repo` time)
- Trim post-walk pipeline (~30 ms BFS + cross-repo crawl) — the floor on warm runs now
- Considered-and-rejected cache shapes (cacache, redb, rkyv, merkle-root-mtime, rayon-parallel-stat) — see TODO for the analysis
