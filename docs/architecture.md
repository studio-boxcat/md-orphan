> **Related:** [[README.md]], [[TODO.md]]

# md-orphan architecture

How the codebase is organized after the cross-repo / cache / style-rule overhaul. Why each cut sits where it does.

## Background

The tool started as a flat single-file Swift CLI: walk the tree, extract links, BFS, report orphans. Adding cross-repo references, link-style canonicalization, fenced-code skipping, and a per-file extraction cache grew `MdOrphan.swift` to ~600 LOC mixing four concerns. The Phase 1 audit surfaced the obvious smells: a 128-line `bfsCrawl` body, two helpers with 10 and 11 parameters and 5 and 4 inout slots respectively, three `MdLink`/`MdLinkDetail`/`CachedLink` structs that all describe the same thing at different lifecycle points.

## Prior art

Three peer tools at comparable scale informed the cut:

- **mlc** (Rust, ~2-3k LOC) — flat module layout: `main.rs`, `cli.rs`, `file_traversal.rs`, `markup.rs`, plus `link_extractors/` and `link_validators/`. Confirms the **extract / validate** seam recurs at this scale ([source](https://github.com/becheran/mlc/tree/master/src)).
- **awesome_bot** (Ruby, ~1.5k LOC) — `lib/awesome_bot/{check,links,output,result,cli}.rb`. Files-as-namespaces, no pipeline abstraction ([source](https://github.com/dkhamsing/awesome_bot/tree/master/lib/awesome_bot)).
- **markdown-link-check** (JS, ~300 LOC `index.js`) — full coordination layer in one file ([source](https://github.com/tcort/markdown-link-check/blob/master/index.js)).
- **lychee** (Rust, ~15k LOC) — directory-per-concern under `lychee-lib/src/{extract,checker,filter,collector}/`. Earns the depth via async + 10+ formats × protocols. Out of scope for us ([source](https://github.com/lycheeverse/lychee/tree/master/lychee-lib/src)).

Borrowed: the **extract / resolve** seam from mlc + flat-file layout from awesome_bot. **Skipped**: lychee's pipeline traits and per-protocol modules.

## Module layout

```
Sources/Lib/
  Util.swift       — path helpers + readFile + readBuffer + scanner-internal helpers (~115 LOC)
  Extract.swift    — Link type + byte-level link/heading/fence scanners            (~290 LOC)
  Crawl.swift      — bfsCrawl + CrawlState + LinkIssue + CrawlOptions + resolveLink (~365 LOC)
  Discovery.swift  — fts walk + RepoIndex + indexRepo                              (~110 LOC)
  Config.swift     — global JSON config + .md-orphan + expandPath                  (~140 LOC)
  Cache.swift      — ExtractionCache (mtime + size + content-hash keyed)           (~200 LOC)
Sources/CLI/
  main.swift       — ArgumentParser command, output rendering, --fix byte rewriter (~250 LOC)
```

6 files, each focused. Total ~1.2k LOC; each file under 400. Library types and public-API surface fit on one screen.

Why not subdirectories: peer tools at our scale stay flat. Adding `Lib/extract/` for one file or `Lib/parsing/` for two has the cohesion penalty of misleading concern names.

Why `Util.swift` (not `Discovery.swift`) for `realPath`/`dirName`/`baseName`/`isExcluded`: these are stdlib-shaped path utilities used everywhere. Putting them under "Discovery" misleads.

## Use cases

Two real consumers, ranked by surface area needed:

1. **CLI (`Sources/CLI/main.swift`)** — drives the whole pipeline. Calls `loadGlobalConfig`, `bfsCrawl(entryPoints:, root:)`, formats `[LinkIssue]`, applies `--fix` by rewriting bytes at `Link.pathStart..<Link.pathEnd`. Re-implements style rendering (`[[…]]` vs `` `…` (repo)``) because the wrapping syntax is a CLI display concern, not a library concern.
2. **Tests (`Tests/MdOrphanTests.swift`, 98 tests)** — uses `@testable import` and asserts on the internal extractors (`extractLinks`, `extractHeadings`), the resolver (`resolveLink`), exclusion matching (`isExcluded`), the BFS results, the cache disk round-trip, and a few config helpers (`expandPath`, `loadProjectIgnore`).

No other consumers. No plugin story, no library users at the moment.

Public API after demotion (18 symbols): `Link`, `LinkIssue`, `CrawlOptions`, `ExtractionCache`, `ExtractedFile`, `RepoIndex`, `GlobalConfig`, `ConfigError`, `bfsCrawl` (two overloads), `indexRepo`, `resolveLink`, `isExcluded`, `realPath`, `dirName`, `baseName`, `loadGlobalConfig`, `loadProjectIgnore`, `defaultConfigPath`, `defaultExcludes`, `expandPath`, `extractLinks` (two overloads), `extractHeadings` (two overloads), `anchorId`, `readFile`.

## Data flow

```
                   CLI (main.swift)
                         │
                         ▼
              bfsCrawl(entryPaths, root, options, cache)
                         │
                         ├─► indexRepo(root)  ──►  RepoIndex { mdFiles, byName }
                         │
                         └─► CrawlState (struct, mutating methods)
                                ├─► seed(entryPaths)
                                ├─► loop: dequeue → cache.read(file) → resolve(link)
                                │                         │
                                │                         ├─► Extract.swift scanners
                                │                         └─► extractLinks([Link]) + extractHeadings
                                │
                                └─► pruneCache()
```

`Link` flows extractor → cache → resolver → `--fix` byte rewriter as one struct. No conversion at boundaries. `Codable` derives the on-disk cache shape directly — no separate `CachedLink` mirror type.

## Crawl state

`bfsCrawl` is a 10-line driver over a `CrawlState` struct. Methods on the struct mutate through `mutating self` rather than passing 5 inout parameters between free functions:

```swift
public func bfsCrawl(entryPaths: [String], index: RepoIndex,
                     options: CrawlOptions, cache: ExtractionCache)
    -> (reachable: Set<ino_t>, issues: [LinkIssue])
{
    var state = CrawlState(entryIndex: index, options: options, cache: cache)
    state.seed(entryPaths)
    while let item = state.dequeue() { state.visit(item) }
    state.pruneCache()
    return (state.reachable, state.issues)
}
```

`CrawlState` is a `struct`, not a `class` — single-threaded, single-owner, no identity, dropped at end of `bfsCrawl`. The Phase 2 review pushed for struct over class to keep the "no shared instance, no statics" property explicit.

Two visited sets:
- `reachable: Set<ino_t>` — entry-repo orphan tracking. Inode-keyed for symlink/hardlink dedup.
- `crossRepoVisited: Set<String>` — canonical paths in cross-repo target trees. They get verified + style-checked but never enter `reachable` (orphan detection is scoped to the entry repo).

## Persisted state

| File | Path | Shape |
|---|---|---|
| Global config | `$XDG_CONFIG_HOME/md-orphan/md-orphan.json` (fallback `~/.config/...`) | `{"repos": {name: path}}` or flat `{name: path}` — both accepted; `$VAR` / `~/` expansion |
| Per-repo ignore | `<repo>/.md-orphan` | gitignore-style line patterns; `#` comments |
| Per-repo cache | `$XDG_CONFIG_HOME/md-orphan/cache/<fnv1a64-of-canonical-root>.json` | one file per indexed repo |

**Cache schema** (`schemaVersion: 2`):

```json
{
  "schemaVersion": 2,
  "displayName": "md-orphan",
  "entries": {
    "README.md": {
      "mtimeNs": 1...,
      "size": 1234,
      "contentHash": 17...,
      "links": [{"kind": {"wiki": {}}, "target": "TODO.md", "fragment": null, "pathStart": 1995, "pathEnd": 2002}],
      "headings": ["overview", "usage"]
    }
  }
}
```

Cache validation requires `(mtime_ns, size, content_hash)` to all match. Schema mismatch → silent invalidate (cache is regenerable).

## Pitfalls — avoided in this design

These bugs were paid for during Phase 1; the structure must keep them out:

1. **macOS `realpath` symlink mismatch** (`/var/folders` ↔ `/private/var/folders`). All paths handed to `CrawlState` are canonicalized once at `init` via `realPath`. Mixing canonical and raw paths breaks `hasPrefix` checks. Touched once during the cross-repo refactor; preserved by the new design.
2. **`scanBacktickRef` runaway**. An unclosed `` ` `` followed by no newline before EOF used to advance to `count + 1`, eating the rest of the file (including any later `[[wiki]]` links on the same row). Fix: scanner returns `i + 1` (advance one byte, treat lone backtick as literal) on every no-close branch — never `end + 1`. See `Extract.swift:scanBacktickRef`.
3. **Global `readBuffer` aliasing**. The buffer at `Util.swift:readBuffer` is process-wide and clobbered by every `readFile` call. Per-file extraction must complete before another `readFile` runs. `ExtractionCache.read` extracts links + headings synchronously inside one `readFile` window; preserve that pattern. Tests using `readFile` are `.serialized` for the same reason.
4. **Cache content drift across parser changes**. Old cached byte offsets can be valid against an unchanged file but reflect the prior (buggy) parser. `cacheSchemaVersion` MUST bump on any scanner output change, not just on JSON-shape changes. The Phase 3 commit B bump (1 → 2) demonstrated this.
5. **Cross-repo `..` escape**. A path like `../docs/foo.md` inside `` ` ` (some-repo) `` escapes the target repo root. Resolution falls back to basename lookup in the target repo's `byName`. The escape is reported as a style violation (canonical form = bare basename), not a hard error. See `Crawl.swift:resolveCrossRepo`.

## Non-goals

- **No `Pipeline` / `Stage` protocol.** We have one of each.
- **No `Reporter` trait.** One binary, two output modes (text + `--fix`); no plugin story.
- **No subdirectory carve-up of `Sources/Lib/`.** Flat at this scale.
- **No async / parallel work.** Single-threaded fts + read is the right shape; reference tools at our scale also don't parallelize. Re-evaluate if profiling identifies a parallelizable hot path.
- **No backwards-compat shims.** When `MdLink` / `MdLinkDetail` / `CachedLink` were collapsed into `Link`, the old types were deleted in the same commit; tests adapted atomically.
- **No standard-link style rule.** `[text](path)` is renderer-relative — applying basename canonicalization would silently break GitHub rendering. Wiki and cross-repo backtick refs only.

## References

- [mlc — closest-scale peer](https://github.com/becheran/mlc/tree/master/src)
- [awesome_bot — flat lib/ at ~1.5k LOC](https://github.com/dkhamsing/awesome_bot/tree/master/lib/awesome_bot)
- [markdown-link-check — coordination in <300 LOC](https://github.com/tcort/markdown-link-check/blob/master/index.js)
- [lychee — what we don't need to be (15k LOC, async, multi-format)](https://github.com/lycheeverse/lychee/tree/master/lychee-lib/src)
- [Swift Forums on TaskGroup overhead at small scales](https://forums.swift.org/t/taskgroup-and-parallelism/51039)
- [APFS dirent traversal benchmarks (FTS_NOSTAT vs alternatives)](http://blog.tempel.org/2019/04/dir-read-performance.html)
- [Why we don't use `getattrlistbulk` (Apple Forums)](https://developer.apple.com/forums/thread/656787)
- [`Swift Regex 28-33× slower than manual scan`](https://forums.swift.org/t/slow-regex-performance/75768)
