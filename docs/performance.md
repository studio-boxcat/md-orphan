> **Related:** [[README.md]], [[architecture.md]]

# Performance

Wall-clock numbers, where time goes, and what the cache and `--no-default-excludes` flags actually buy you. Re-measured after the Phase 3 architecture refactor; numbers unchanged from pre-overhaul.

## Test bench

- macOS arm64 (M-series), APFS local volume, release build (`swift build -c release`)
- Best-of-3 wall clock; first run of each scenario discarded (FS warmup ~1s extra)
- **Self-check**: this repo, 4 `.md` files, entry `AGENTS.md`
- **Unity-scale**: meow-tower repo, ~109k files (Library/Pods excluded by defaults), 134 reachable `.md` files

## Wall clock

| Scenario | Time | Notes |
|---|---|---|
| Self-check (4 `.md`) | **~7 ms** | startup + fts + 4 file reads |
| Unity-scale, no cache | **~500 ms** | every run does fts + read + extract |
| Unity-scale, cold cache (write) | **~560 ms** | +60 ms to write cache JSON |
| Unity-scale, warm cache (read) | **~490 ms** | extraction skipped, cache validation costs ~hash time |
| Unity-scale, `--no-default-excludes` | **~1.4 s** | walks `Library/`, `Pods/`, etc. — file count goes from 109k to ~190k |

## Where time goes (Unity-scale)

Per-phase breakdown via `benchmarkMeowTower` test (release):

| Phase | Time | What's happening |
|---|---|---|
| `indexRepo` (defaults on) | ~390 ms | fts walk over 109k entries; the kernel `readdir` + per-`.md` `stat()` for inode |
| `indexRepo` (defaults off) | ~1.4 s | +Library/Pods/etc., file count 109k → 144k inodes in `byName` |
| Read + extract 134 `.md` | ~7-13 ms | `read(2)` into reusable buffer + manual byte scanner. ~0.07 ms/file |
| BFS resolve + style + output | ~80-100 ms | path resolution, basename lookups, issue rendering |

The fts walk dominates by ~30×. **Reading and extracting markdown is essentially free** at this scale.

## Why the cache is break-even here

The cache exists, is correct (mtime + size + fnv1a64 content hash, atomic writes, schema-versioned), and shaves the 7–13 ms extraction step on subsequent runs. But:

- The validation path **always reads the file and hashes it** — that's the bulk of what a fresh extraction costs.
- Hashing 600 KB of `.md` content is microseconds; extraction itself is also microseconds.
- Net wall-clock difference between cold-no-cache and warm-with-cache is within run-to-run variance (~10 ms).

Where the cache earns its keep:

- Larger doc trees (1000s of `.md` files) — extraction starts to add up.
- Per-file content hashes catch the "post-`--fix` re-run with byte-equal output" scenario where `mtime + size` could be unchanged but cached byte offsets are stale.

Disable with `--no-cache` to skip cache machinery entirely.

## Parallel cross-repo discovery

When the entry files reference cross-repo targets directly, those repos are indexed in parallel via `DispatchQueue.concurrentPerform`. Wall-clock cost for N **first-level** referenced repos is `max(walk_time)` instead of `sum(walk_time)`.

The prefetch reads the seeded entry files in `CrawlState.seed`, extracts cross-repo names, and dispatches `indexRepo` for those targets in parallel. **Transitively-discovered cross-repos** (refs from a cross-repo file rather than the entry, or from same-repo files reachable from the entry) fall back to lazy serial via `indexFor`.

Measured on meow-tower (`~/Develop/meow-tower/CLAUDE.md`):

| Scenario | Time | Where parallel engages |
|---|---|---|
| meow-tower entry, transitive cross-repos to 3 real repos | 510-660 ms | not engaged — entry CLAUDE.md has 0 first-level cross-repo refs; meow-toolbox/meow-game-server/meow-dev-media discovered transitively from `docs/specs/*` and walked serially |
| Synthetic entry directly referencing 3 cross-repos | 525-640 ms | engaged — 105% CPU on first run, 3 parallel walks |

The wins are modest in practice because most projects reference cross-repos transitively (cross-repo refs scattered across the doc tree, not concentrated in the entry file). For projects that DO concentrate cross-repo refs in the entry CLAUDE.md, parallel discovery saves the sum of cross-repo walk times.

For projects with no cross-repo refs anywhere, prescan finds zero targets and no parallel work runs.

Tracked in [[TODO.md]]: extending to transitive cross-repos via level-synchronous BFS could save ~100-200 ms more on meow-tower-like setups, at the cost of trading FIFO crawl order for level-by-level batching.

## What dominates

1. **Per-file `String` allocations during the walk** — was the original 810 ms regression. Fixed in `Discovery.swift` by skipping non-`.md` `String(cString:)`/`dropFirst` work before allocation. 810 ms → 540 ms.
2. **fts dirent traversal cost** — kernel-side, no user-space optimization fixes this. Default excludes skip the big subtrees (`Library/`, `Pods/`, `node_modules/`, `.build/`, `DerivedData/`).
3. ~~**Per-`.md` `stat(2)`**~~ — **removed.** `RepoIndex.mdFiles` is now a `Set<String>` of relpaths; orphan dedup uses canonical paths via `realPath` lazily on un-matched candidates only. Net wall-clock change is in the noise (saved ~1ms in indexRepo, paid back as ~0–1ms in the orphan filter for the rare symlinked `.md`).

## What doesn't help

- **Async / parallel work**: TaskGroup spawn cost dominates for sub-ms per-file work. mlc, awesome_bot, markdown-link-check at our scale all stay single-threaded ([Swift Forums thread](https://forums.swift.org/t/taskgroup-and-parallelism/51039)).
- **`getattrlistbulk`**: known macOS bugs ([Apple Forums](https://developer.apple.com/forums/thread/656787)).
- **`Swift Regex`**: 28-33× slower than the manual byte scanner ([Swift Forums](https://forums.swift.org/t/slow-regex-performance/75768)).

## Reproducing

CLI wall clock (best-of-3):

```
swift build -c release
for i in 1 2 3; do (time .build/release/md-orphan --no-cache /path/to/repo/CLAUDE.md > /dev/null) 2>&1 | head -1; done
```

Per-phase breakdown (un-disable the test):

```
# In Tests/MdOrphanTests.swift, change `@Test(.disabled(...))` → `@Test` on benchmarkMeowTower
swift test -c release --filter benchmarkMeowTower
# Re-disable when done.
```

The benchmark hardcodes `~/Develop/meow-tower` — adapt for other repos.

## Tracked perf follow-ups

See [[TODO.md]] for:

- Extending parallel discovery to transitive cross-repos (level-synchronous BFS)
- Directory-level mtime cache (skip `readdir` for unchanged dirs)
- Non-`.md` style support cost analysis (~30× current `indexRepo` time)
