# md-orphan

Detect markdown files not reachable from a given entry point by crawling relative links recursively.

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
md-orphan <entry-point...> [--exclude <prefix>...] [--verbose]
```

```
md-orphan CLAUDE.md
md-orphan CLAUDE.md README.md
md-orphan --exclude Library,Packages AGENTS.md
md-orphan --verbose CLAUDE.md
```

The root directory is the parent of the entry point. All `.md` files under that directory are scanned. Silent on success by default — only outputs orphan list on failure (exit 1).

| Flag | Description |
|------|-------------|
| `--exclude <pattern>` | Exclude paths by prefix or glob (comma-separated, repeatable) |
| `--verbose`, `-v` | Show success message when all files are reachable |

## Structure

- `Sources/Lib/` — Core library (discovery, link extraction, BFS crawl)
- `Sources/CLI/` — ArgumentParser entry point
- `Tests/` — Swift Testing test suite
- `dist/` — Pre-built release binary

## Algorithm

1. **Discover** — Find all `.md` files under root, keyed by inode (`stat.st_ino`) for correct symlink/hardlink handling
2. **Crawl** — BFS from entry points, extracting `[text](path)` and `[[wiki]]` links (supports `#anchor` stripping, `|alias`, `./` and `../` relative paths; ignores URLs and extensionless links). `.md` links are followed for further crawling; non-`.md` links (images, PDFs, etc.) are checked for existence only. Paths resolved relative to containing file, normalized, and bounded to root
3. **Diff** — Report files whose inodes are not in the reachable set

Edge cases: missing entry point → exit 1; broken link (`.md` or non-`.md`) → exit 1; circular links → inode visited set; multiple entry points → union; symlinks → followed

