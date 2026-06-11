> **Related:** [[CLAUDE.md]], [[TODO.md]], [[rust-migration.md]]

# md-orphan architecture

How the codebase is organized today (Rust, post Swift→Rust migration). Why each cut sits where it does.

## Background

The tool started life as a single-file Swift CLI. It accumulated cross-repo references, link-style canonicalization, fenced-code skipping, a per-file extraction cache, and parallel cross-repo discovery before being ported to Rust for cross-platform reach + a tighter walker. The structural choices below predate the language port — see [[rust-migration.md]] for the migration record.

## Prior art

Three peer tools at comparable scale informed the cut:

- **mlc** (Rust, ~2-3k LOC) — flat module layout: `main.rs`, `cli.rs`, `file_traversal.rs`, `markup.rs`, plus `link_extractors/` and `link_validators/`. Confirms the **extract / validate** seam recurs at this scale ([source](https://github.com/becheran/mlc/tree/master/src)).
- **awesome_bot** (Ruby, ~1.5k LOC) — `lib/awesome_bot/{check,links,output,result,cli}.rb`. Files-as-namespaces, no pipeline abstraction ([source](https://github.com/dkhamsing/awesome_bot/tree/master/lib/awesome_bot)).
- **markdown-link-check** (JS, ~300 LOC `index.js`) — full coordination layer in one file ([source](https://github.com/tcort/markdown-link-check/blob/master/index.js)).
- **lychee** (Rust, ~15k LOC) — directory-per-concern under `lychee-lib/src/{extract,checker,filter,collector}/`. Earns the depth via async + 10+ formats × protocols. Out of scope for us ([source](https://github.com/lycheeverse/lychee/tree/master/lychee-lib/src)).

Borrowed: the **extract / resolve** seam from mlc + flat-file layout from awesome_bot. **Skipped**: lychee's pipeline traits and per-protocol modules.

## Module layout

```
src/
  path.rs       — path helpers + CanonicalPath + read_file/atomic_write_bytes + scanner-internal helpers
  exclude.rs    — ExcludeMatcher + DEFAULT_EXCLUDES + libc fnmatch FFI
  extract.rs    — Link type + byte-level link/heading/fence scanners
  crawl.rs      — bfs_crawl + CrawlState + LinkIssue + apply_style_fixes
  discovery.rs  — index_repo + RepoIndex (`ignore::WalkParallel`-based, parallel)
  config.rs     — global JSON config + .md-orphan + expand_path
  cache.rs      — ExtractionCache (mtime + size + content-hash keyed)
  walk_cache.rs — WalkCache: persisted RepoIndex, per-dir-mtime validated
  main.rs       — clap-derive command + output rendering + --fix wiring
```

Each file stays focused on one concern; `crawl.rs` is the heaviest (BFS + resolver + style-fix rewriter + cross-repo crawl). Library types and public-API surface fit on one screen.

Why not subdirectories: peer tools at our scale stay flat. Adding `src/extract/` for one file or `src/parsing/` for two has the cohesion penalty of misleading concern names.

Why `path.rs` (not `discovery.rs`) for `real_path` / `dir_name` / `base_name`: these are stdlib-shaped path utilities used everywhere. Putting them under "Discovery" misleads.

## Use cases

Two real consumers, ranked by surface area needed:

1. **CLI (`main.rs`)** — drives the whole pipeline. Calls `load_global_config`, `bfs_crawl_at_root`, formats `[LinkIssue]`, applies `--fix` by rewriting bytes at `Link.path_start..Link.path_end`. Re-implements style rendering (`[[…]]` vs `` `…` (repo)``) because the wrapping syntax is a CLI display concern, not a library concern.
2. **Tests (per-module `#[cfg(test)]` blocks)** — assert on the byte scanners (`extract_links`, `extract_headings`), the resolver (`resolve_link`), the matcher (`ExcludeMatcher`), the BFS results, the cache disk round-trip, and config helpers (`expand_path`, `load_project_ignore`).

No other consumers. No plugin story, no library users at the moment.

Public API surface is whatever each module exports with `pub` — grep `^pub ` to enumerate. Stable extension points are the types named in the data-flow diagram below; everything else is incidental.

## Data flow

```
                   CLI (main.rs)
                         │
                         ▼
              bfs_crawl_at_root(entry_paths, root, options, cache)
                         │
                         ├─► index_repo(root)  ──►  RepoIndex { md_files, by_name }
                         │
                         └─► CrawlState (struct, &mut self methods)
                                ├─► seed(entry_paths)
                                │     └─► prefetch_referenced_repos (std::thread::scope, parallel)
                                ├─► loop: dequeue → cache.read(file) → resolve(link)
                                │                         │
                                │                         ├─► extract.rs scanners
                                │                         └─► extract_links([Link]) + extract_headings
                                │
                                └─► prune_cache()
```

`Link` flows extractor → cache → resolver → `--fix` byte rewriter as one struct. No conversion at boundaries. `serde::Serialize`/`Deserialize` derive the on-disk cache shape directly — no separate `CachedLink` mirror type.

## Crawl state

`bfs_crawl` is a 6-line driver over a `CrawlState` struct. Methods on the struct mutate through `&mut self` rather than passing parameters between free functions:

```rust
pub fn bfs_crawl(
    entry_paths: &[String],
    index: RepoIndex,
    options: &CrawlOptions,
    cache: &mut ExtractionCache,
) -> (HashSet<CanonicalPath>, Vec<LinkIssue>) {
    let mut state = CrawlState::new(index, options.clone(), cache);
    state.seed(entry_paths);
    while let Some(item) = state.dequeue() { state.visit(&item); }
    state.prune_cache();
    (state.reachable, state.issues)
}
```

`CrawlState` is a struct (with a `'c` lifetime borrowing `&mut ExtractionCache`) — single-threaded, single-owner, dropped at end of `bfs_crawl`. No statics, no shared instance.

Two visited sets, both keyed by canonical absolute path — the `CanonicalPath` newtype (`path.rs`) brands `realpath(3)` output so raw paths can't sneak into identity checks (Pitfall #1 made compile-checked). `RepoIndex.by_name` values stay unbranded `String`s: the walk doesn't resolve file-level symlinks, so they're only canonical after `basename_fallback` re-realpaths them.
- `reachable: HashSet<CanonicalPath>` — entry-repo orphan tracking. Files in this set get their outgoing links resolved + recursively followed. Marked *before* the file is read: an unreadable file is still reachable (reported as an `Unreadable` issue, not an orphan).
- `cross_repo_visited: HashSet<CanonicalPath>` — cross-repo target files the entry repo directly references. They're read so anchor checks against their headings work, but their outgoing links are NOT followed — `visit()` returns early after caching headings. Issues from cross-repo files would be downstream-repo concerns, not entry-repo concerns.

Symlinks pointing to the same `.md` resolve to one canonical via `std::fs::canonicalize`, so path-based dedup handles them. Hardlinks (rare in doc trees) are processed twice.

## Persisted state

| File | Path | Shape |
|---|---|---|
| Global config | `$XDG_CONFIG_HOME/md-orphan/md-orphan.json` (fallback `~/.config/...`) | `{"repos": {name: path}}` or flat `{name: path}` — both accepted; `$VAR` / `~/` expansion |
| Per-repo ignore | `<repo>/.md-orphan` | gitignore-style line patterns; `#` comments. Required at the entry repo root. |
| Per-repo cache | `$XDG_CONFIG_HOME/md-orphan/cache/<fnv1a64-of-canonical-root>.json` | one file per indexed repo |
| Per-repo walk-cache | `$XDG_CONFIG_HOME/md-orphan/walk-cache/<fnv1a64-of-canonical-root>.json` | persisted `RepoIndex` + per-dir mtime map |

**Per-file cache schema** (`schema_version: 5`, defined in `cache.rs`):

```json
{
  "schema_version": 5,
  "display_name": "md-orphan",
  "repo_set_hash": 12345678901234567890,
  "entries": {
    "README.md": {
      "mtime_ns": 1700000000000000000,
      "size": 1234,
      "content_hash": 17000000000000000000,
      "links": [{"kind": "Wiki", "target": "TODO.md", "fragment": null, "path_start": 1995, "path_end": 2002}],
      "headings": ["overview", "usage"]
    }
  }
}
```

Per-entry validation: `(mtime_ns, size, content_hash)` all match. Per-cache validation: `repo_set_hash` matches the active configured repo set (cross-repo refs are filtered against that set at extract time, so cached entries become unsafe when the set changes). Schema mismatch on either → silent invalidate. Bumps:
- 2→3 during the Rust port (serde tagged-enum shape diverged from Swift Codable)
- 3→4 added `repo_set_hash` to track repo-set changes
- 4→5 scanner emits `LinkKind::Inline` for plain backtick spans (v4 entries lack them)
- 5→6 inline-span guards stop at `#` — raw-text fragments with spaces now emitted

**Walk-cache schema** (`WALK_CACHE_SCHEMA_VERSION: 1`, defined at `walk_cache.rs`):

```json
{
  "schema_version": 1,
  "canonical_root": "/Users/.../meow-tower",
  "flags_key": 12345678901234567890,
  "dir_mtimes": {"": 1700000000000000000, "docs": 1700000000000000001},
  "md_files": ["README.md", "docs/architecture.md"],
  "by_name": {"README.md": ["/abs/path/README.md"]},
  "effective_exclude": ["Library/", "Pods/"]
}
```

Validation: schema match + `flags_key` match + every `dir_mtimes` entry stats unchanged. Any miss → fall through to a fresh walk. `flags_key` is `fnv1a64(use_default_excludes | include_all_extensions | sorted-effective-excludes)`; bump triggers for the schema version live in the const's doc comment.

## Pitfalls — avoided in this design

These bugs were paid for during the Swift era; the structure must keep them out:

1. **macOS `canonicalize` symlink mismatch** (`/var/folders` ↔ `/private/var/folders`). All paths handed to `CrawlState` are canonicalized once at construction. Mixing canonical and raw paths breaks `starts_with` checks.
2. **`scan_backtick_ref` runaway**. An unclosed `` ` `` with no newline before EOF used to advance past end-of-buffer, eating later `[[wiki]]` links on the same row. Fix: scanner returns `i + 1` (advance one byte, treat lone backtick as literal) on every no-close branch — never `end + 1`. See `extract.rs:scan_backtick_ref`.
3. **Cache content drift across parser changes**. Old cached byte offsets can be valid against an unchanged file but reflect the prior (buggy) parser. `CACHE_SCHEMA_VERSION` MUST bump on any scanner output change, not just on JSON-shape changes.
4. **Cross-repo `..` escape**. A path like `../docs/foo.md` inside `` ` ` (some-repo) `` escapes the target repo root. Resolution falls back to basename lookup in the target repo's `by_name`. The escape is reported as a style violation (canonical form = bare basename), not a hard error. See `crawl.rs:resolve_cross_repo`.
5. **`anchor_id` Unicode parity**. Swift `Character.isLetter` iterates grapheme clusters; Rust `char` is a Unicode scalar. Rust port uses `unicode_segmentation::UnicodeSegmentation::graphemes(true)` to match Swift's behavior on decomposed `é`, Korean precomposed/decomposed jamo, ZWJ emoji clusters. Verified against `anchor_id_parity.tsv` captured from the Swift binary.

## Non-goals

- **No `Pipeline` / `Stage` trait.** We have one of each.
- **No `Reporter` trait.** One binary, two output modes (text + `--fix`); no plugin story.
- **No subdirectory carve-up of `src/`.** Flat at this scale.
- **No async work.** Single-threaded walk + read is the right shape; cross-repo discovery uses `std::thread::scope` for the rare 3+-repo prefetch case. Reference tools at our scale also don't go async.
- **No backwards-compat shims.** When the cache schema bumped 2→3 during the Rust port, old caches invalidated on first run; no Swift-format reader was kept around.
- **No standard-link style rule.** `[text](path)` is renderer-relative — applying basename canonicalization would silently break GitHub rendering. Wiki and cross-repo backtick refs only.

## References

- [Rust port migration record](rust-migration.md)
- [mlc — closest-scale peer](https://github.com/becheran/mlc/tree/master/src)
- [awesome_bot — flat lib/ at ~1.5k LOC](https://github.com/dkhamsing/awesome_bot/tree/master/lib/awesome_bot)
- [markdown-link-check — coordination in <300 LOC](https://github.com/tcort/markdown-link-check/blob/master/index.js)
- [lychee — what we don't need to be (15k LOC, async, multi-format)](https://github.com/lycheeverse/lychee/tree/master/lychee-lib/src)
- [ignore — Rust gitignore-aware parallel walker (also used by ripgrep, fd)](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore)
- [unicode-segmentation — grapheme cluster iteration](https://github.com/unicode-rs/unicode-segmentation)
