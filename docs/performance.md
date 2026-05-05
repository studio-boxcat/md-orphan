> **Related:** [[README.md]], [[architecture.md]]

# Performance

Wall-clock numbers, where time goes, and what the cache and `--no-default-excludes` flags actually buy you. Re-measured after Swift→Rust migration; numbers improved ~30% over the Swift baseline.

## Test bench

- macOS arm64 (M-series), APFS local volume, release build (`cargo build --release`)
- Best-of-3 wall clock; first run of each scenario discarded (FS warmup ~1s extra)
- **Self-check**: this repo, ~6 `.md` files, entry `AGENTS.md`
- **Unity-scale**: meow-tower repo, ~51k files (Library/Pods/proj-*/Packages excluded by defaults + `.md-orphan`), 116 reachable `.md` files

## Wall clock (Rust binary, post-sort-removal)

| Scenario | Time | Notes |
|---|---|---|
| Self-check (~7 `.md`) | **~3-5 ms** | startup + walk + 7 file reads |
| Unity-scale, no cache | **~165-200 ms** | best ~164 ms; sys-time variance from APFS FS cache state |
| Unity-scale, cold cache (write) | **~225 ms** | +60 ms to write cache JSON |
| Unity-scale, warm cache (read) | **~165 ms** | extraction skipped, cache validation costs ~hash time |
| Unity-scale, `--no-default-excludes` | **~1.0 s** | walks `Library/`, `Pods/`, etc. — file count goes from 51k to ~190k |

Comparison points:

| Tool | Time on meow-tower (51k pruned) |
|---|---|
| md-orphan (current Rust) | ~165 ms |
| Swift baseline pre-port | ~225 ms |
| `fd '' --threads 1` (single-threaded walk only) | ~180-210 ms |
| `fd ''` (parallel default, walk only) | ~60-70 ms |

md-orphan ≈ matches `fd --threads 1` on the walk; the gap to `fd` parallel is ~3× and would require parallelizing the walker (see [[TODO.md]]).

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

1. **walkdir traversal cost** — kernel-side. Default excludes skip the big subtrees (`Library/`, `Pods/`, `node_modules/`, `.build/`, `DerivedData/`); per-repo `.md-orphan` adds project-specific prunes.
2. **`ExcludeMatcher` bare-basename hash lookup** — O(1) per dir entry. Was the original 153 ms hot path in Swift before precompiled patterns; Rust port keeps the same precompile + `HashSet` pattern.

## What doesn't help

- **rayon / `tokio` async**: thread spawn overhead dominates for sub-ms per-file work. Reference tools at our scale (mlc, awesome_bot, markdown-link-check) all stay single-threaded.
- **`getattrlistbulk`** (macOS-specific batched stat): known kernel bugs ([Apple Forums](https://developer.apple.com/forums/thread/656787)).
- **regex**: a manual byte scanner is faster than `regex` for our tight scanner pattern (single-char-class transitions).

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
- Walk-result cache (per-dir mtime validation; could land warm runs at <30 ms)
- Non-`.md` style support cost analysis (~10× current `index_repo` time)
