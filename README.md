# md-orphan

Detect markdown files not reachable from a given entry point by crawling relative links recursively.

## Install

Pre-built macOS binary in `dist/`. Build and symlink to `/usr/local/bin`:

```
just install
```

Or build only:

```
just build
```

## Usage

```
md-orphan <entry-point...> [--verbose]
```

```
md-orphan CLAUDE.md
md-orphan CLAUDE.md README.md
md-orphan --verbose CLAUDE.md    # show success message when all files are reachable
```

The root directory is the parent of the entry point. All `.md` files under that directory are scanned. Silent on success by default — only outputs orphan list on failure (exit 1).

| Flag | Description |
|------|-------------|
| `--verbose`, `-v` | Show success message when all files are reachable |

## Algorithm

1. **Discover** — Glob all `.md` files under the entry point's parent directory, index by inode
2. **Crawl** — BFS from the entry point(s), extracting markdown links and resolving them to canonical paths
3. **Diff** — Report discovered files whose inodes are not in the reachable set

### File Identity

Uses **inode number** (`stat.st_ino`) as the unique file identifier:
- Visited set and reachable set keyed by inode, not path strings
- Handles symlinks and hardlinks correctly
- Avoids path canonicalization overhead

### Link Extraction

Extracts relative `.md` links from markdown syntax:
- `[text](path.md)` — standard links
- `[text](path.md#anchor)` — strip fragment
- `[text](./path.md)` — explicit relative
- `[text](../dir/path.md)` — parent traversal

Ignores:
- URLs (`http://`, `https://`)
- Non-markdown links (`.png`, `.ts`, etc.)
- HTML `<a>` tags (out of scope)

### Path Resolution

Links are relative to the file containing them. Resolve to canonical paths:
- `docs/system/langpack.md` contains `[guide](../dev/guide.md)` → `docs/dev/guide.md`
- Normalize `..` and `.` segments
- Paths must stay within the root directory (no escape)

### Edge Cases

- Entry point doesn't exist → error with exit 1
- Broken link (target doesn't exist) → warn to stderr, don't count as orphan
- Circular links → inode-based visited set, don't revisit
- Multiple entry points → union the reachable sets
- Symlinks → follow by default

## Implementation

Swift 5.9+, single-threaded, no Foundation. POSIX `fts`/`open`/`fstat`/`read` for file I/O, manual UTF-8 byte scanning for link extraction (no regex), `swift-argument-parser` for CLI. Target: < 10ms for ~200 files on local SSD.

## Performance Research

Key findings that informed the implementation choices above.

### Directory Traversal: `fts_open` is fastest on APFS

[Tempelmann's benchmarks](http://blog.tempel.org/2019/04/dir-read-performance.html) (updated Feb 2025) across 357K files on local APFS:

| Method | Time | Notes |
|--------|------|-------|
| `fts_open` (optimized flags) | 4.610s | `FTS_PHYSICAL \| FTS_NOCHDIR \| FTS_NOSTAT` |
| `enumeratorAtURL` (empty keys) | 4.818s | Close, but more Foundation overhead |
| `enumeratorAtURL` (nil keys) | 9.075s | 2x slower — fetches all attributes |
| `enumeratorAtPath` | 13.590s | Slowest Foundation option |

Flag choice matters: with default flags (0), fts is slower than `enumeratorAtURL`. `FTS_NOSTAT` skips stat on every entry; we only call `stat()` (via `fstat`) on `.md` files.

Avoid `getattrlistbulk` — [Apple DTS calls it "notoriously tricky"](https://developer.apple.com/forums/thread/656787) with alignment pitfalls and an APFS bug.

### File Reading: `read()` beats `mmap` for small files

For files read once sequentially (1-50KB typical for markdown), [read() outperforms mmap](https://medium.com/cosmos-code/mmap-vs-read-a-performance-comparison-for-efficient-file-access-3e5337bd1e25) because mmap's setup cost (VMAs, page table entries, page faults) is not amortized. Using `fstat()` on the open fd gets the inode and file size in one syscall, avoiding a separate `stat()` call.

### Swift Regex: Do Not Use

[Swift Regex has severe performance problems](https://forums.swift.org/t/slow-regex-performance/75768) — 28-33x slower than Java's regex. The bottleneck is [excessive retain/release and COW array copying](https://forums.swift.org/t/why-is-regex-slower-than-using-a-bunch-of-string-contains/66247). Fixes require `~Copyable` and `UTF8Span` integration, not yet shipped.

For a well-defined pattern like `[text](path.md)`, manual byte scanning through `UnsafeBufferPointer<UInt8>` is dramatically faster — the structural characters (`[`, `]`, `(`, `)`) are all ASCII, so we scan raw bytes and only allocate `String` for actual `.md` link targets.

### Concurrency: Overhead Exceeds Benefit at This Scale

[TaskGroup overhead](https://forums.swift.org/t/taskgroup-and-parallelism/51039) includes heap allocation per task, cooperative thread pool scheduling, and context switching. At ~200 small files where each `open`/`read`/`close` takes ~10-50μs, the scheduling overhead alone exceeds the I/O time. BFS is also inherently sequential (new files to visit come from the current file's links).

### Build Optimization

[Swift optimization tips](https://github.com/swiftlang/swift/blob/main/docs/OptimizationTips.rst): SwiftPM release mode already enables `-O` and whole-module optimization. Adding `-cross-module-optimization` enables cross-target inlining. Use `final`/`private`/structs to enable devirtualization. [Noncopyable types](https://infinum.com/blog/swift-non-copyable-types/) (`~Copyable`, Swift 5.9+) eliminate retain/release for owned buffers.

## Stretch Goals

- `--exclude <pattern>`: Glob patterns to exclude (repeatable)
- `--dot` output: generate Graphviz DOT of the link graph
