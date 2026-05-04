# TODO

## Deferred follow-ups

### Cross-repo parser: false-positive on inline-code annotations
The `` `path.ext` (name) `` cross-repo grammar collides with inline-code annotations the user actually writes — e.g. `` `Unity.Analytics` (Runtime) ``, `` `LangProvider.Lang` (PlayerPrefs-backed) ``, `` `UISortingOrder.Popup` (30) ``. On meow-tower these surface as 30+ "unknown cross-repo references" errors that aren't real refs. Fix: thread the configured repo names into the parser; only emit `Link.Kind.crossRepo` when the parenthesized name matches a known repo. Otherwise treat the span as an inline code span (no link emitted, no error). This makes the resolver-level `unknownRepo` issue fire only on actual typos in cross-repo refs, not on every parenthesized annotation.

### Inline-code style check
Detect `` `path.ext` `` (no `(repo)` suffix) inline code spans and apply the same canonical-form rule as wiki links. **FP guards required**:
- Skip if backtick span contains shell metachars (`*`, `?`, `$`, `|`, `<`, `>`)
- Skip if backtick span contains whitespace (commands like `` `git status` ``)
- Skip if no extension AND no `/`
- Resolve target via repo basename map; only flag when there's a real file match

The fenced-code skipper is already in place, so this can be added to `scanBacktickRef` without parser-level changes.

### Non-`.md` style support
`indexRepo` only puts `.md` files into `byName` by default — populating it for all extensions in a Unity-sized repo (~100k files) costs ~800ms vs ~30ms. Wiki/cross-repo style for `.cs`, `.swift`, etc. currently goes unchecked.

Hooks already in place: `indexRepo(includeAllExtensions: true)` produces a complete map. To wire up:
1. Plumb a CrawlOptions flag through to `indexRepo` per repo
2. Decide on user-facing surface: a global `--all-extensions-style` flag, or auto-detect by file extension when a non-`.md` style violation might apply
3. Document the perf trade-off

### Possible: extend parallel discovery to transitive cross-repos
The current prefetch only catches first-level cross-repo refs in the seeded entry files. Cross-repo targets discovered deeper (e.g. meow-tower's `docs/specs/*` referencing `meow-game-server`) fall back to lazy serial walks. Could switch BFS to level-synchronous and parallel-batch all cross-repo refs at each level boundary. Win on meow-tower-like setups: ~100-200 ms (sum of ~3 transitive cross-repo walks → max). Trade-off: FIFO crawl order becomes level-order. Defer until profiling shows the lazy serial fallback dominating.

### Possible: directory-level mtime cache
Currently the fts walk runs on every invocation. If profiling shows it dominating runtime in large monorepos with dense ignore lists already applied, we can layer a per-directory mtime cache (skip `readdir` for unchanged dirs, still stat each dir). Defer until profiling justifies it.

### Concurrent invocation safety
Cache writes are last-writer-wins. If users start running two `md-orphan` invocations against the same repo simultaneously (e.g. editor save hooks + lefthook), consider `flock(2)` on the cache file. So far accepted as last-writer-wins because cache is regenerable.

### XDG nit
`expandPath` understands `$VAR` and leading `~/`. It does not currently expand `~user/` (other-user homes). Nobody asked, but worth mentioning.

### Multiple entry points spanning sibling dirs
`main.swift:67` does `let root = dirName(resolvedEntries[0])` — if the user passes `a/index.md b/index.md` where `a/` and `b/` are siblings, the second entry is treated as living under `a/`'s root (broken-link-prone). Either (a) reject multi-entry across sibling dirs, or (b) compute a common ancestor. Pre-existing edge case; not a regression.

