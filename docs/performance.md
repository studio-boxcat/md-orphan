> **Related:** [[README.md]], [[architecture.md]]

# Performance

Wall-clock numbers, where time goes, and what the cache and `--no-default-excludes` flags actually buy you. Re-measured after Swift→Rust migration; numbers improved ~30% over the Swift baseline.

## Test bench

- macOS arm64 (M-series), APFS local volume, release build (`cargo build --release`)
- Best-of-3 wall clock; first run of each scenario discarded (FS warmup ~1s extra)
- **Self-check**: this repo, ~6 `.md` files, entry `AGENTS.md`
- **Unity-scale**: meow-tower repo, ~51k files (Library/Pods/proj-*/Packages excluded by defaults + `.md-orphan`), 116 reachable `.md` files

## Wall clock (Rust binary, parallel walker via `ignore::WalkParallel`)

| Scenario | Time | Notes |
|---|---|---|
| Self-check (~7 `.md`) | **~7-8 ms** | startup + thread pool spinup + 7 file reads. Slight regression from ~3 ms (sequential) — acceptable trade for the meow-tower win below. |
| Unity-scale, no cache | **~105-130 ms** | best ~103 ms; ~500% CPU (5-core parallelism) |
| Unity-scale, cold cache (write) | **~165 ms** | +60 ms to write cache JSON |
| Unity-scale, warm cache (read) | **~105-145 ms** | extraction skipped, cache validation costs ~hash time |
| Unity-scale, `--no-default-excludes` | **~700 ms** | walks `Library/`, `Pods/`, etc. — parallelism saves more on bigger trees |

Progression on meow-tower:

| Stage | Time | Δ |
|---|---|---|
| Swift baseline (pre-Rust) | ~225 ms | — |
| Rust port (sequential `walkdir`) | ~165 ms | -27% |
| + drop `sort_by_file_name` + precompute root prefix | ~165 ms (best 164 ms) | wash on wall clock; -30 ms user time |
| + `ignore::WalkParallel` (5 worker threads) | **~105 ms (best 103 ms)** | -36% |

| Tool | Walk-only time on meow-tower (51k pruned) |
|---|---|
| `fd ''` (parallel default) | ~37 ms |
| md-orphan walker (~30-50 ms of the 105 ms total) | comparable to fd |
| `fd '' --threads 1` | ~180-210 ms |

md-orphan's full pipeline is ~105 ms, of which ~30-50 ms is walk and the rest is read + extract + BFS resolve + render. The walker itself now matches `fd` parallel.

## Where time goes (Unity-scale)

- `index_repo` (defaults on, walkdir + ExcludeMatcher fast path) — ~110-130 ms. Dominated by the kernel `getdents` traversal of ~51k entries; Rust's per-entry overhead (no `String(cString:)` allocation, no `String.contains` in the prune hot path) is much lighter than Swift fts was.
- Read + extract 116 `.md` files — ~3-7 ms. Manual byte scanner; ~0.05 ms/file.
- BFS resolve + style + output — ~30-50 ms. Path resolution, basename lookups, issue rendering.

The walk dominates by ~10×. **Reading and extracting markdown is essentially free** at this scale.

## Why the cache is break-even here

The cache exists, is correct (mtime + size + fnv1a64 content hash, atomic writes via `tempfile`, schema-versioned), and shaves the ~5 ms extraction step on subsequent runs. But:

- The validation path **always reads the file and hashes it** — that's the bulk of what a fresh extraction costs.
- Hashing 600 KB of `.md` content is microseconds; extraction itself is also microseconds.
- Net wall-clock difference between cold-no-cache and warm-with-cache is within run-to-run variance (~5 ms).

Where the cache earns its keep:

- Larger doc trees (1000s of `.md` files) — extraction starts to add up.
- Per-file content hashes catch the "post-`--fix` re-run with byte-equal output" scenario where `mtime + size` could be unchanged but cached byte offsets are stale.

Disable with `--no-cache` to skip cache machinery entirely.

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
- Walk-result cache (per-dir mtime validation; could land warm runs at <30 ms — the only step left to genuinely beat fd)
- Non-`.md` style support cost analysis (~10× current `index_repo` time)
