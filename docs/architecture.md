> **Related:** [[README.md]], [[TODO.md]]

# md-orphan architecture (Phase-2 draft)

End-state design before refactor. Phase 4 finalizes against what shipped.

## Module layout

```
Sources/Lib/
  Extract.swift     — byte-level link/heading/fence scanners + Link, Heading types
  Crawl.swift       — bfsCrawl + LinkIssue + CrawlOptions + LinkResolver state
  Discovery.swift   — fts walk + RepoIndex + path utilities (realPath, dirName, baseName, isExcluded)
  Config.swift      — global JSON, .md-orphan, expandPath
  Cache.swift       — ExtractionCache (per-repo content-hash keyed)
Sources/CLI/
  main.swift        — ArgumentParser command, output rendering, --fix byte rewriter
```

5 lib files, each focused. Down from current 4 files where the largest (`MdOrphan.swift`, 600 LOC) mixes utilities + scanning + resolution + BFS.

Why not subdirectories: at ~1.5k LOC, peer tools (mlc ~2k, awesome_bot ~1.5k) all stay flat. lychee directories its `lychee-lib/src/` but earns it with async + 10+ formats — not our problem. ([lychee source](https://github.com/lycheeverse/lychee/tree/master/lychee-lib/src), [mlc source](https://github.com/becheran/mlc/tree/master/src))

## Type unification: one Link

Today three structs describe the same thing at three lifecycle points:

- `MdLink { path, fragment }` — public test surface
- `MdLinkDetail { path, fragment, kind: LinkKind, pathStart, pathEnd }` — extraction output
- `CachedLink { path, fragment, kindTag, crossRepoName, pathStart, pathEnd }` — cache form

Collapse to one:

```swift
struct Link: Codable, Equatable {
    enum Kind: Codable, Equatable {
        case wiki                       // [[target]]
        case standard                   // [text](target)
        case crossRepo(repo: String)    // `target` (repo)
    }
    let kind: Kind
    let target: String           // path before fragment
    let fragment: String?        // anchor or nil
    let pathStart: Int           // byte offset of `target` within source file
    let pathEnd: Int             // exclusive
}
```

One struct flows extractor → cache → resolver → `--fix` byte rewriter. Codable derives the cache shape automatically. The convenience `extractLinks(from: String) -> [String]` API stays for tests / external callers; internally everything is `[Link]`.

## Crawl state encapsulation

Today `bfsCrawl` is 128 lines with three nested closures (`indexFor`, `repoRootContaining`, `headings`), and dispatches to `processSameRepoLink` (10 params, 5 inout) / `processCrossRepoLink` (11 params, 4 inout). Inout-soup that's hard to extend.

Replace with a `CrawlState` reference-typed value:

```swift
final class CrawlState {
    let entryRoot: String
    let resolvedRepos: [String: String]
    let options: CrawlOptions
    let cache: ExtractionCache

    var queue: [QueueItem] = []
    var queuedEntryPaths: Set<String> = []
    var crossRepoVisited: Set<String> = []
    var reachable: Set<ino_t> = []
    var issues: [LinkIssue] = []
    var headingCache: [String: Set<String>] = [:]
    var indices: [String: RepoIndex] = [:]

    func enqueue(_ canonical: String, owningRoot: String)
    func record(_ issue: LinkIssue)
    func headings(for canonical: String) -> Set<String>
    func indexFor(_ canonicalRoot: String) -> RepoIndex
    func repoRootContaining(_ path: String) -> String?
}
```

`bfsCrawl` becomes a 30-line driver:

```swift
func bfsCrawl(entryPaths: [String], root: String, options: CrawlOptions, cache: ExtractionCache)
    -> (reachable: Set<ino_t>, issues: [LinkIssue])
{
    let state = CrawlState(entryRoot: root, options: options, cache: cache)
    state.seed(entryPaths)
    while let item = state.dequeue() {
        guard let extracted = state.read(item) else { continue }
        for link in extracted.links { state.resolve(link, in: item) }
    }
    state.pruneCache()
    return (state.reachable, state.issues)
}
```

`state.resolve(link, in:)` dispatches on `Link.Kind` to small private methods. Each is ~30 lines because the state mutations are method calls instead of inout params.

## Discovery unification

Delete `discoverFiles`. Make `bfsCrawl` always walk via `indexRepo`. The legacy `allFiles:` parameter on `bfsCrawl` (currently kept for test/back-compat) goes away — tests adapt to `bfsCrawl(entryPaths:, root:, options:)`.

`indexRepo` keeps its current `.md`-only `byName` default + `includeAllExtensions` opt-in (deferred non-`.md` style support — see [[TODO.md]]).

## Path resolution

Both `resolveLink` (relative-to-source) and the cross-repo path-walking inside `processCrossRepoLink` walk segments and strip `..`. Extract a single helper:

```swift
/// Normalize segments, return nil if the result escapes constraintRoot.
func normalizeWithinRoot(_ rawCombined: String, constraintRoot: String) -> String?
```

Same algorithm, two callers: `resolveSameRepo` (combined = sourceDir + link) and `resolveCrossRepo` (combined = repoRoot + link).

Fix the latent symlink bug surfaced in audit: cross-repo roots from config get `realPath` once during `CrawlState.init`, not lazily on lookup. Eliminates the unresolved/canonical mismatch.

## Public vs internal surface

Currently several internals are `public` only because tests reach for them. Tests already use `@testable import MdOrphanLib` — they can see `internal` symbols. Demote:

| Symbol | Now | After |
|---|---|---|
| `MdLinkDetail` | public | deleted (replaced by `Link`) |
| `LinkKind` | public | folded into `Link.Kind` |
| `extractLinksDetailed` | public | internal |
| `cacheDirectory`, `cacheFilePath` | public | internal |
| `fnv1a64`, `fnv1a64Hex` | public | internal |
| `homeDir` | public | internal |
| `registerDisplayName` | public | deleted (unused) |
| `loadLinkCache`, `saveLinkCache` | public | internal |
| `MdLink` | public | replaced by `Link` (also public) |

Public stays: `bfsCrawl`, `CrawlOptions`, `LinkIssue`, `ExtractionCache`, `extractLinks(from:)`, `extractLinksWithFragments(from:)`, `extractHeadings(from:)`, `resolveLink`, `isExcluded`, `realPath`, `dirName`, `baseName`, `loadGlobalConfig`, `loadProjectIgnore`, `defaultConfigPath`, `defaultExcludes`, `RepoIndex`, `indexRepo`.

## Persisted state

- **Cache JSON**: shape unchanged (mtimeNs, size, contentHash, links[], headings[]). `Link.Codable` produces the same field names → no on-disk migration. Bump `cacheParserVersion` 2 → 3 so any in-the-wild caches written by the old parser are invalidated regardless.
- **Global config JSON**: unchanged.
- **`.md-orphan`**: unchanged.

## Pitfalls preserved (don't lose these in refactor)

These bugs were paid for in the current code; the new design must not lose the fix:

1. `realpath` mismatch on macOS `/var/folders` ↔ `/private/var/folders` — root + entry paths must be canonicalized at `CrawlState.init` (already done for entry root + cross-repo roots).
2. Unclosed-backtick scanner regression — `scanBacktickRef` must `return i + 1` (advance one byte) when no closing backtick is found, never `return end + 1` (would eat past end-of-buffer).
3. `readBuffer` is a process-global; per-file extraction must complete before another `readFile` call. `ExtractionCache.read` already extracts immediately; preserve.
4. `parserVersion` must bump on ANY scanner change, not just structural ones — old caches may have correct mtime+size+hash but wrong byte offsets if the scanner reordered output.

## Non-goals

- No `Pipeline` / `Stage` protocol. We have one of each.
- No `Reporter` trait — one binary, two output modes (text + `--fix`), no plugin story.
- No subdirectory carve-up of `Sources/Lib/`. Flat at this scale.
- No move toward async/parallel. Single-threaded fts + read is the right shape; the reference tools at our scale also don't parallelize.
- No backwards-compat shim for `MdLink`/`MdLinkDetail` removal — tests update atomically.

## Open questions

- Whether to keep `discoverFiles` as a thin alias for `indexRepo(...).mdFiles` for one release as a compatibility hint. Recommend: no — delete cleanly.
- Whether `extractLinks(from: String)` etc. convenience APIs add value when CLI doesn't use them. Tests do. Keep until proven dead.
- Whether `CLI/main.swift` should grow a `runOrphanCheck` library entry that the CLI delegates to. Use-case audit said the formatting + `--fix` are CLI-shaped; not worth pulling in. Defer.

## References

- [lychee — async link checker, ~15k LOC](https://github.com/lycheeverse/lychee/tree/master/lychee-lib/src)
- [mlc — closest-scale peer, ~2-3k LOC](https://github.com/becheran/mlc/tree/master/src)
- [markdown-link-check — coordination layer in <300 LOC](https://github.com/tcort/markdown-link-check/blob/master/index.js)
- [awesome_bot — flat lib/ at ~1.5k LOC](https://github.com/dkhamsing/awesome_bot/tree/master/lib/awesome_bot)
