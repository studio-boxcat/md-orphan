# md-orphan

Detect markdown files not reachable from a given entry point by crawling links recursively. Also flags broken links, ambiguous basenames, broken anchors, and link-style violations — including cross-repo references resolved through a global config.

## Install

Build into `dist/` and symlink to `~/.local/bin`:

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
| `--fix` | Rewrite link style issues in place (atomic write); a fix that can't be written exits 1 |
| `--config <path>` | Override global config (default `$XDG_CONFIG_HOME/md-orphan/md-orphan.json`) |
| `--no-default-excludes` | Disable built-in defaults (`.git`, `node_modules`, `Library`, `.build`, ...) |
| `--no-cache` | Disable both the walk-result cache and the per-file extraction cache |
| `--all-extensions` | Index every extension so non-`.md` wiki refs (`[[foo.cs]]`) get style + basename resolution (~30× walk cost on Unity-scale repos) |
| `--orient` | Print md-orphan's own `CLAUDE.md` (usage guide for this tool) |

## Link styles

The tool recognizes three link forms in markdown. Style violations are flagged when a link could be expressed in a more canonical form, where canonical = **bare basename** when the basename is unique within its target repo, or **root-relative path** when not. Wiki paths resolve source-relative first, then root-relative (Obsidian semantics), then by unique basename — so both canonical forms always resolve.

| Form | Example | Style-checked? |
|---|---|---|
| Wiki | `[[guide.md]]`, `[[guide.md#sec\|alias]]`, `[[#section]]` | yes (any extension) |
| Standard md link | `[text](path.md)` | broken/ambiguous/anchor only — no style rewrite |
| Cross-repo backtick | `` `bar.md` (meow-toolbox) ``, `` `bar.md#sec` (repo) `` | yes |

Standard md links (`[text](path)`) get broken-link / ambiguity / anchor checks, but are **not** rewritten — most renderers (GitHub, etc.) interpret them as filesystem-relative, so basename-magic would silently break them.

**Cross-repo annotation filter:** the `` `path.ext` (name) `` syntax is only treated as a cross-repo ref when `name` matches a configured repo. Patterns like `` `view.name` (GridView) ``, `` `Unity.Analytics` (Runtime) ``, `` `UISortingOrder.Activity` (10) `` are silently treated as plain inline code. A plain backtick span without a `(repo)` suffix is never a link — it's prose, regardless of how path-like it looks. Trade-off: typos to a known-repo name are caught at file-resolution (`CrossRepoBroken`); typos to a wrong-repo name are silent.

**Anchor fragments:** wiki and cross-repo fragments accept either the kebab-case slug (`#content-type`) or the raw heading text (`#Content Type` — Obsidian convention); raw text is slugified before lookup. Standard md link fragments must be the exact slug — renderers resolve them as real URL fragments, where raw text 404s.

**Self-anchor links:** a wiki link with an empty path and a fragment (`[[#section]]`, `[[#section|alias]]`) targets the *current* file. The fragment is validated against the source file's own headings (broken → `BrokenAnchor`); there's no path to resolve, style-check, or rewrite. The standard-link equivalent `[text](#section)` is **not** checked — it has no extension, so it's dropped at the parser like any other extensionless link.

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

`$VAR` / `${VAR}` and a leading `~/` (or `~user/`) are expanded against the environment. Default location: `$XDG_CONFIG_HOME/md-orphan/md-orphan.json`, falling back to `~/.config/md-orphan/md-orphan.json`. Override with `--config <path>`.

Failure modes (all exit 1): file doesn't exist in target repo, style violation, broken anchor. A `` `…` (name) `` whose name isn't in the config is treated as plain inline code, not a cross-repo ref — see the parser filter note in [Link styles](#link-styles).

The crawl visits each cross-repo target file the entry repo *directly* references — to verify the file exists and its anchors resolve — but **does not recurse** into the cross-repo file's own outgoing links. Cross-repo internal rot is the responsibility of that repo's own md-orphan run, not yours. Orphan detection is also scoped to the entry repo only.

## Per-repo ignore (`.md-orphan`)

**Required.** Every entry repo must have a `.md-orphan` file at its root listing project-specific ignore patterns. Running md-orphan against a repo without one — or with one that exists but can't be read — exits 2 with a clear error message. If you have nothing to add beyond the built-in defaults, an empty file (`touch .md-orphan`) satisfies the requirement.

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

Two per-repo layers under `$XDG_CONFIG_HOME/md-orphan/` — a **walk-result cache** (skips the repo walk while directory mtimes are unchanged; invalidated by changes to `--exclude`, `.md-orphan`, or `--no-default-excludes`) and a **per-file extraction cache** (skips link/heading extraction for unmodified files, validated by mtime + size + content hash). Both are schema-versioned and atomically written; load errors and corrupted files silently fall through to fresh work. Schemas, keys, and validation rules: [[architecture.md#persisted-state]].

Disable both with `--no-cache`.

## Structure

Flat one-module-per-concern `src/` — layout, per-module responsibilities, and design rationale: [[architecture.md#module-layout]]. `dist/md-orphan` is the locally built release binary (gitignored; `just build` refreshes it, `~/.local/bin/md-orphan` symlinks to it). Benchmarks: [[performance.md]]. Historical Swift→Rust migration record: [[rust-migration.md]].

## Algorithm

1. **Discover** — `ignore::WalkParallel` traversal under the entry root with per-thread visitor pruning excluded subtrees (work-stealing across `num_cpus` threads). `.md` filenames enter the basename map for style/ambiguity checks; `--all-extensions` widens the map to every file (~30× walk cost on Unity-sized repos, off by default).
2. **Crawl** — BFS from entry points. For each visited entry-repo file: extract links (cached when source unchanged), resolve each link, check broken/ambiguous/anchor/style. Cross-repo refs trigger lazy index of the target repo. The target file is visited (heading extraction for anchor checks) but its outgoing links are NOT followed — cross-repo recursion stops at depth 1.
3. **Diff** — `.md` files in the entry repo whose canonical path is not in the reachable set are orphans.

Edge cases: missing entry point → error (exit 2); broken link → exit 1; unreadable file → `Unreadable` issue (exit 1; still counted reachable, never doubles as an orphan); circular links → visited set; symlinks → `std::fs::canonicalize` (handles macOS `/var/folders` → `/private/var/folders`); multiple entry points → reachability union (all must live under the first entry's root — siblings are rejected with exit 2).

## Performance

~5 ms self-check; ~63 ms cold / ~29 ms warm on a Unity-scale 51k-file repo (post-prune). Numbers, per-phase breakdown, and what the walk-cache and per-file cache actually buy: [[performance.md]].
