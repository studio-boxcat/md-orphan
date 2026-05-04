# TODO

## Deferred follow-ups

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

### Possible: directory-level mtime cache
Currently the fts walk runs on every invocation. If profiling shows it dominating runtime in large monorepos with dense ignore lists already applied, we can layer a per-directory mtime cache (skip `readdir` for unchanged dirs, still stat each dir). Defer until profiling justifies it.

### Possible: parallel cross-repo discovery
Cross-repo crawls walk each referenced repo serially. If we routinely span 5+ repos, parallelizing the initial `indexRepo` calls (one per cross-repo target) could cut wall-clock noticeably. Trivial via `DispatchGroup` or Swift Concurrency.

### Concurrent invocation safety
Cache writes are last-writer-wins. If users start running two `md-orphan` invocations against the same repo simultaneously (e.g. editor save hooks + lefthook), consider `flock(2)` on the cache file. So far accepted as last-writer-wins because cache is regenerable.

### XDG nit
`expandPath` understands `$VAR` and leading `~/`. It does not currently expand `~user/` (other-user homes). Nobody asked, but worth mentioning.
