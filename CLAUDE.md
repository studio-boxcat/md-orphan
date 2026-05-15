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
| `--no-cache` | Disable both the walk-result cache and the per-file extraction cache |
| `--orient` | Print md-orphan's own `CLAUDE.md` (usage guide for this tool) |

## Link styles

The tool recognizes four link forms in markdown. Style violations are flagged when a link could be expressed in a more canonical form, where canonical = **bare basename** when the basename is unique within its target repo, or **root-relative path** when not.

| Form | Example | Style-checked? |
|---|---|---|
| Wiki | `[[guide.md]]`, `[[guide.md#sec\|alias]]` | yes (any extension) |
| Standard md link | `[text](path.md)` | broken/ambiguous/anchor only — no style rewrite |
| Cross-repo backtick | `` `bar.md` (meow-toolbox) ``, `` `bar.md#sec` (repo) `` | yes |
| Inline code | `` `path.ext` `` (no repo suffix) | deferred — see [[TODO.md]] |

Standard md links (`[text](path)`) get broken-link / ambiguity / anchor checks, but are **not** rewritten — most renderers (GitHub, etc.) interpret them as filesystem-relative, so basename-magic would silently break them.

**Cross-repo annotation filter:** the `` `path.ext` (name) `` syntax is only treated as a cross-repo ref when `name` matches a configured repo. Patterns like `` `view.name` (GridView) ``, `` `Unity.Analytics` (Runtime) ``, `` `UISortingOrder.Activity` (10) `` are silently treated as inline-code annotations. Trade-off: typos to a known-repo name are caught at file-resolution (`CrossRepoBroken`); typos to a wrong-repo name are silent.

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

Failure modes (all exit 1): file doesn't exist in target repo, style violation, broken anchor. A `` `…` (name) `` whose name isn't in the config is treated as an inline-code annotation, not a cross-repo ref — see the parser filter note in [Link styles](#link-styles).

The crawl visits each cross-repo target file the entry repo *directly* references — to verify the file exists and its anchors resolve — but **does not recurse** into the cross-repo file's own outgoing links. Cross-repo internal rot is the responsibility of that repo's own md-orphan run, not yours. Orphan detection is also scoped to the entry repo only.

## Per-repo ignore (`.md-orphan`)

**Required.** Every entry repo must have a `.md-orphan` file at its root listing project-specific ignore patterns. Running md-orphan against a repo without one exits 1 with a clear error message. If you have nothing to add beyond the built-in defaults, an empty file (`touch .md-orphan`) satisfies the requirement.

Loaded automatically for the entry repo and every cross-repo target visited during recursion. Cross-repo targets without their own `.md-orphan` fall back to defaults only — no hard-fail on cross-repo absence.

The "root" is the parent directory of the first entry point. When that parent has no `.md-orphan` but a strict ancestor does, the missing-file error reframes as "wrong-scoped entry point — pass an entry point inside `<ancestor>` instead" rather than suggesting you create a phantom mini-root at the subdirectory. Create `.md-orphan` at the subdirectory only when it's genuinely a separate repo with its own scope.

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

## Caches

Two layers, both keyed by `fnv1a64(canonical_root)` — two repos with the same basename in different parents don't collide. Both use atomic writes (tempfile + rename), schema-versioned, last-writer-wins on concurrent invocations.

**Walk-result cache** at `$XDG_CONFIG_HOME/md-orphan/walk-cache/<hash>.json` — persists `RepoIndex` (md_files + by_name + effective excludes). Validation: per-dir mtime stat (APFS bumps dir mtime on entry add/remove/rename, not file content edits). Flags-keyed: changes to `--exclude`, `.md-orphan`, `--no-default-excludes` invalidate. On hit, skips the entire `index_repo` walk (~99 ms cold → ~40 ms warm on Unity-scale repos).

**Per-file extraction cache** at `$XDG_CONFIG_HOME/md-orphan/cache/<hash>.json` — caches links + headings per `.md` file. Per-entry validation: `(mtime_ns, size, fnv1a64(content))` all match. Per-cache-file validation: `repo_set_hash` (fnv1a64 of sorted configured repo names) — invalidates the whole cache when the user's repo config changes, since cross-repo refs are filtered against that set at extract time. Catches the post-`--fix` byte-equal-output edge case via content hash. Entries for vanished files auto-pruned each run.

Load errors silently fall through to fresh extraction; corrupted files are treated as misses and overwritten next run.

Disable both with `--no-cache`.

## Structure

- `src/path.rs` — path helpers (`real_path`, `dir_name`, `base_name`, `rel_path`) + `read_file`
- `src/exclude.rs` — `ExcludeMatcher` with bare-basename hash-set fast path + `DEFAULT_EXCLUDES`
- `src/extract.rs` — `Link` type + byte-level link/heading/fence scanners + grapheme-aware `anchor_id`
- `src/crawl.rs` — `bfs_crawl`, `CrawlState`, `LinkIssue`, `CrawlOptions`, `resolve_link`, `apply_style_fixes`
- `src/discovery.rs` — `index_repo` + `RepoIndex` (`ignore::WalkParallel`-based)
- `src/config.rs` — global JSON config + per-repo `.md-orphan` parsing + `expand_path`
- `src/cache.rs` — per-file extraction cache (mtime + size + fnv1a64 content-hash keyed)
- `src/walk_cache.rs` — walk-result cache: persisted `RepoIndex`, per-dir-mtime validated
- `src/main.rs` — clap-derive CLI entry + output rendering + `--fix` wiring
- `tests/fixtures/` — anchor-id parity TSV captured during the Swift→Rust port
- `dist/md-orphan` — pre-built release binary (committed to repo for fast `just install`)
- See [[architecture.md]] for module layout + design rationale, [[performance.md]] for benchmarks, and [[rust-migration.md]] for the historical Swift→Rust migration record

## Algorithm

1. **Discover** — `ignore::WalkParallel` traversal under the entry root with per-thread visitor pruning excluded subtrees (work-stealing across `num_cpus` threads). `.md` filenames enter the basename map for style/ambiguity checks. (Non-`.md` extensions in the basename map costs ~30× more on Unity-sized repos and is off by default — see [[TODO.md]].)
2. **Crawl** — BFS from entry points. For each visited entry-repo file: extract links (cached when source unchanged), resolve each link, check broken/ambiguous/anchor/style. Cross-repo refs trigger lazy index of the target repo. The target file is visited (heading extraction for anchor checks) but its outgoing links are NOT followed — cross-repo recursion stops at depth 1. Two visited sets, both keyed by canonical path.
3. **Diff** — `.md` files in the entry repo whose canonical path is not in the reachable set are orphans.

Edge cases: missing entry point → exit 1; broken link → exit 1; circular links → visited set; symlinks → `std::fs::canonicalize` (handles macOS `/var/folders` → `/private/var/folders`); multiple entry points → reachability union.

## Performance

~5 ms self-check; ~63 ms cold / ~29 ms warm on a Unity-scale 51k-file repo (post-prune). Numbers, per-phase breakdown, and what the walk-cache and per-file cache actually buy: [[performance.md]].
