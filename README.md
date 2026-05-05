# md-orphan

Detect markdown files not reachable from a given entry point by crawling links recursively. Also flags broken links, ambiguous basenames, broken anchors, and link-style violations — including cross-repo references resolved through a global config.

## Install

Pre-built macOS binary in `dist/`. Build and symlink to `~/.local/bin`:

```
just install
```

Or build only:

```
just build
```

## Usage

```
md-orphan <entry-point...> [flags]
```

```
md-orphan CLAUDE.md
md-orphan CLAUDE.md README.md
md-orphan --verbose CLAUDE.md
md-orphan --fix CLAUDE.md
```

The root directory is the parent of the entry point. All `.md` files under that directory are scanned. Silent on success by default — only outputs issues on failure (exit 1).

| Flag | Description |
|------|-------------|
| `--exclude <pattern>` | Exclude paths by prefix or glob (comma-separated, repeatable) |
| `--verbose`, `-v` | Show success message when all files are reachable |
| `--fix` | Rewrite link style issues in place (atomic write) |
| `--config <path>` | Override global config (default `$XDG_CONFIG_HOME/md-orphan/md-orphan.json`) |
| `--no-default-excludes` | Disable built-in defaults (`.git`, `node_modules`, `Library`, `.build`, ...) |
| `--no-cache` | Disable the per-file extraction cache |
| `--claude` | Print full path + contents of nearest `CLAUDE.md` (walks up from cwd) |

## Link styles

The tool recognizes four link forms in markdown. Style violations are flagged when a link could be expressed in a more canonical form, where canonical = **bare basename** when the basename is unique within its target repo, or **root-relative path** when not.

| Form | Example | Style-checked? |
|---|---|---|
| Wiki | `[[guide.md]]`, `[[guide.md#sec\|alias]]` | yes (any extension) |
| Standard md link | `[text](path.md)` | broken/ambiguous/anchor only — no style rewrite |
| Cross-repo backtick | `` `bar.md` (meow-toolbox) ``, `` `bar.md#sec` (repo) `` | yes |
| Inline code | `` `path.ext` `` (no repo suffix) | deferred — see [[TODO.md]] |

Standard md links (`[text](path)`) get broken-link / ambiguity / anchor checks, but are **not** rewritten — most renderers (GitHub, etc.) interpret them as filesystem-relative, so basename-magic would silently break them.

Fenced code blocks (` ``` `) are skipped during scanning — content inside fences is never parsed as a link or cross-repo ref.

### Style examples

```
[[../system/foo.md]]               → [[foo.md]]                  (basename unique in repo)
[[docs/system/foo.md]]             → [[foo.md]]                  (basename unique in repo)
[[a/foo.md]] (with b/foo.md)       → unchanged (basename duplicated; root-relative is canonical)

`docs/foo.md` (meow-toolbox)        → `foo.md` (meow-toolbox)     (basename unique in target repo)
`../docs/foo.md` (meow-tower)       → `foo.md` (meow-tower)       (path escape; basename fallback)
```

Pass `--fix` to rewrite the source bytes in place. The replacement is scoped to the path bytes only — fragments, aliases, and the `(repo)` suffix are preserved.

## Cross-repo configuration

Cross-repo refs `` `path.ext` (repo-name) `` are resolved by looking up the repo name in a global config file. Two equivalent JSON shapes are accepted:

```json
{
  "repos": {
    "meow-tower":   "$HOME/Develop/meow-tower",
    "meow-toolbox": "$HOME/Develop/meow-toolbox"
  }
}
```

```json
{
  "meow-tower":   "$HOME/Develop/meow-tower",
  "meow-toolbox": "$HOME/Develop/meow-toolbox"
}
```

`$VAR` / `${VAR}` and a leading `~/` are expanded against the environment. Default location: `$XDG_CONFIG_HOME/md-orphan/md-orphan.json`, falling back to `~/.config/md-orphan/md-orphan.json`. Override with `--config <path>`.

Failure modes (all exit 1): cross-repo ref to a repo not in config, file doesn't exist in target repo, style violation, broken anchor.

The crawl follows cross-repo `.md` targets recursively — links inside those files are checked too. **Orphan detection** is scoped to the entry repo only; cross-repo files are visited and verified but never participate in orphan reachability.

## Per-repo ignore (`.md-orphan`)

**Required.** Every entry repo must have a `.md-orphan` file at its root listing project-specific ignore patterns. Running md-orphan against a repo without one exits 1 with a clear error message. If you have nothing to add beyond the built-in defaults, an empty file (`touch .md-orphan`) satisfies the requirement.

Loaded automatically for the entry repo and every cross-repo target visited during recursion. Cross-repo targets without their own `.md-orphan` fall back to defaults only — no hard-fail on cross-repo absence.

```
# Comments and blank lines are ignored.
Pods/                       # bare basename — matches at ANY depth (proj-ios/Pods/ etc.)
Packages/
docs/draft-*.md
docs/internal/              # path-anchored — only matches at root
```

Pattern syntax (gitignore-flavored):

- Trailing `/` makes it a directory pattern.
- **Bare basename + trailing `/`** (`Pods/`, `Library/`) — matches that directory **at any depth** in the tree.
- **Path-containing + trailing `/`** (`docs/internal/`) — anchored at the repo root.
- Patterns with `*`, `?`, `[…]` are matched as `fnmatch(3)` globs (PATHNAME mode — `*` doesn't cross `/`).
- Plain patterns (no `/`, no glob) match as path prefix at root.
- No negation. Use CLI `--exclude` to add CLI-time patterns.

Built-in defaults (`.git`, `.svn`, `.hg`, `node_modules`, `.build`, `DerivedData`, `Library`, `Pods`, `target`, `vendor`, `.venv`, `__pycache__`) apply on top and use the same nested-matching semantics. Disable with `--no-default-excludes`.

## Cache

Per-file extraction (links + headings) is cached at `$XDG_CONFIG_HOME/md-orphan/cache/<hash>.json`, one file per indexed repo. Filename is `fnv1a64(canonical_root)` — two repos with the same basename in different parents don't collide.

**Validation**: `(mtime_ns, size, fnv1a64(content))` must all match the on-disk file before a cache entry is reused. Mismatch → re-extract + update.

**Robustness**: a single `cacheSchemaVersion` field invalidates cache on any format or parser change; atomic writes (tmp + rename via the `tempfile` crate); load errors silently fall through to fresh extraction; entries for files no longer in the repo are auto-pruned each run.

**Concurrency**: last-writer-wins on concurrent invocations. Cache is regenerable, so corruption is non-fatal — a corrupted file is treated as a miss and overwritten on next run.

Disable with `--no-cache`.

## Structure

- `src/path.rs` — path helpers (`real_path`, `dir_name`, `base_name`, `rel_path`) + `read_file`
- `src/exclude.rs` — `ExcludeMatcher` with bare-basename hash-set fast path + `DEFAULT_EXCLUDES`
- `src/extract.rs` — `Link` type + byte-level link/heading/fence scanners + grapheme-aware `anchor_id`
- `src/crawl.rs` — `bfs_crawl`, `CrawlState`, `LinkIssue`, `CrawlOptions`, `resolve_link`, `apply_style_fixes`
- `src/discovery.rs` — `index_repo` + `RepoIndex` (walkdir-based)
- `src/config.rs` — global JSON config + per-repo `.md-orphan` parsing + `expand_path`
- `src/cache.rs` — per-file extraction cache (mtime + size + fnv1a64 content-hash keyed)
- `src/main.rs` — clap-derive CLI entry + output rendering + `--fix` wiring
- `tests/fixtures/` — anchor-id parity TSV captured during the Swift→Rust port
- `dist/md-orphan` — pre-built release binary (committed to repo for fast `just install`)
- See [[architecture.md]] for module layout + design rationale, [[performance.md]] for benchmarks, and [[rust-migration.md]] for the historical Swift→Rust migration record

## Algorithm

1. **Discover** — `walkdir` traversal under the entry root with `filter_entry` pruning excluded subtrees. `.md` filenames enter the basename map for style/ambiguity checks. (Non-`.md` extensions in the basename map costs ~30× more on Unity-sized repos and is off by default — see [[TODO.md]].)
2. **Crawl** — BFS from entry points. For each visited file: extract links (cached when source unchanged), resolve each link, check broken/ambiguous/anchor/style. Cross-repo refs trigger lazy index of the target repo and recursive crawl. Two visited sets, both keyed by canonical path.
3. **Diff** — `.md` files in the entry repo whose canonical path is not in the reachable set are orphans.

Edge cases: missing entry point → exit 1; broken link → exit 1; circular links → visited set; symlinks → `std::fs::canonicalize` (handles macOS `/var/folders` → `/private/var/folders`); multiple entry points → reachability union.

## Performance

~7 ms self-check, ~500 ms on a Unity-scale 109k-file repo with defaults applied. Numbers, per-phase breakdown, and what the cache actually buys: [[performance.md]].
