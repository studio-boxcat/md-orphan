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
md-orphan <entry-point...> [--exclude <prefix>...] [--verbose]
```

```
md-orphan CLAUDE.md
md-orphan CLAUDE.md README.md
md-orphan --exclude Library --exclude Packages AGENTS.md
md-orphan --verbose CLAUDE.md
```

The root directory is the parent of the entry point. All `.md` files under that directory are scanned. Silent on success by default — only outputs orphan list on failure (exit 1).

| Flag | Description |
|------|-------------|
| `--exclude <prefix>` | Exclude paths matching prefix, repeatable |
| `--verbose`, `-v` | Show success message when all files are reachable |

## Algorithm

1. **Discover** — Find all `.md` files under root, keyed by inode (`stat.st_ino`) for correct symlink/hardlink handling
2. **Crawl** — BFS from entry points, extracting `[text](path.md)` links (supports `#anchor` stripping, `./` and `../` relative paths; ignores URLs and non-`.md` links). Paths resolved relative to containing file, normalized, and bounded to root
3. **Diff** — Report files whose inodes are not in the reachable set

Edge cases: missing entry point → exit 1; broken link → warn to stderr; circular links → inode visited set; multiple entry points → union; symlinks → followed

