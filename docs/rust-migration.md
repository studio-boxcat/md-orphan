> **Related:** [[architecture.md]], [[performance.md]], [[../README.md]]

# Rust migration plan (draft)

End-state design for porting md-orphan from Swift to Rust. Round-2 design after Round-1 research.

## Why Rust

Honest assessment, not perf-justified alone:

- **Cross-platform**: opens Linux CI runners, Windows-bonus
- **Smaller binary**: ~5MB Swift → ~2MB Rust
- **Better walker ecosystem**: `walkdir` + `ignore` are best-in-class
- **Simpler concurrency**: `rayon::par_iter` vs `DispatchQueue.concurrentPerform`
- **Tighter byte-slice paths**: no `String(cString:)` allocations per fts entry
- **Cargo + crates.io**: better dependency story than SwiftPM at this scale

What this **doesn't** buy us once walk-cache lands: cold-walk speed (warm cache = 12ms regardless of language).

## Crate layout

```
md-orphan/  (renamed crate; binary still `md-orphan`)
├── Cargo.toml
├── Cargo.lock
├── src/
│   ├── main.rs            — clap derive, CLI driver, output rendering, --fix wiring (~320 LOC)
│   ├── lib.rs             — re-exports + crate-level docs
│   ├── path.rs            — real_path, dir_name, base_name, rel_path, read_file (~180 LOC)
│   ├── exclude.rs         — ExcludeMatcher, default_excludes (~120 LOC)
│   ├── extract.rs         — Link, scanners, anchor_id, extract_headings (~320 LOC)
│   ├── discovery.rs       — RepoIndex, index_repo (~160 LOC)
│   ├── config.rs          — GlobalConfig, ConfigError, expand_path, project_ignore (~180 LOC)
│   ├── cache.rs           — ExtractionCache, LinkCache, fnv1a64 (~240 LOC)
│   └── crawl.rs           — bfs_crawl, CrawlState, LinkIssue, apply_style_fixes (~420 LOC)
├── tests/
│   └── integration.rs     — Swift Testing → #[test] (~900 LOC, 102 tests)
├── benches/               — criterion benches (deferred, not Round-1)
├── docs/                  — copied over from Swift repo
├── .md-orphan
├── README.md
└── ...
```

Total: ~2900 LOC Rust + tests (~16% larger than Swift; mostly path/error verbosity).

## Dependencies (`Cargo.toml`) — trimmed per Round-2 review

```toml
[package]
name = "md-orphan"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[dependencies]
clap = { version = "4.5", features = ["derive"] }   # 8 flags justify derive macros
anyhow = "1.0"                                       # binary error context
serde = { version = "1", features = ["derive"] }
serde_json = "1"
walkdir = "2"
tempfile = "3"                                       # atomic writes for --fix

[dev-dependencies]
tempfile = "3"

[profile.release]
lto = "thin"
codegen-units = 1
strip = true
```

**Dropped per reviewer pushback** (replaceable with std/hand-rolled):
- `thiserror` — `anyhow` covers our needs; lib errors stay simple
- `globset` — 0-3 globs typically; libc `fnmatch` via FFI or hand-rolled (~30 LOC)
- `memchr` — byte iteration is fast enough at 600KB total content
- `dirs` — `env::var("XDG_CONFIG_HOME").or_else(...)` is 5 LOC
- `rayon` — `std::thread::scope` for 3-5 parallel tasks is 20 LOC
- `pretty_assertions` — vanilla `assert_eq!` is fine

## Naming conventions

- Swift `camelCase` → Rust `snake_case` for fns/fields
- Swift nested types (`LinkIssue.Kind`, `LinkIssue.StyleScope`) → flattened: `IssueKind`, `StyleScope` at module level
- `_swift` suffix avoided; Rust naming wins everywhere

## Key design decisions

### 1. Per-call `Vec<u8>` for file reads (not global buffer)

Swift had a global `readBuffer` reused across calls. Rust idiom: `std::fs::read(path) -> io::Result<Vec<u8>>`. Per-call allocation is sub-ms for our file sizes. Tests run in parallel (no shared state). The `@Suite(.serialized)` constraint disappears.

### 2. JSON cache: bump schema 2→3, no compat dance

Round-2 reviewer correctly flagged: Swift `Codable` for `case crossRepo(repo: String)` emits `{"crossRepo":{"repo":"…"}}` (externally tagged); Rust serde adjacently/internally tagged variants emit different shapes. **They are not interoperable.** Custom Deserialize to accept both is fragile and untestable in the long tail.

Cache is regenerable. Bump `cacheSchemaVersion` 2→3 in Rust port. Existing caches silently invalidate on first run — one-time miss, harmless. Document in CHANGELOG.

### 3. fnv1a64 byte-compat

Swift `String(h, radix: 16)` and Rust `format!("{:x}", h)` both produce lowercase, no leading zeros — byte-identical. Algorithm is portable (FNV-1a constants are deterministic). Cache file *names* (which use the hex hash) survive migration.

### 4. Config JSON shape compat (wrapped vs flat)

Custom `Deserialize` for `GlobalConfig`: parse to `serde_json::Value` first, branch on `"repos"` key presence. Same logic as current Swift impl, ~30 LOC.

### 5. walkdir over fts

`walkdir::WalkDir::new(root).into_iter().filter_entry(|e| !excluded(e))` replaces fts. `filter_entry` skips entire subtrees just like `fts_set(FTS_SKIP)`. Symlink loop detection on by default. Single-threaded baseline ~3× faster than fts (per benchmarks).

### 6. Concurrency via rayon

`prefetchReferencedRepos` uses `DispatchQueue.concurrentPerform`. Replace with:

```rust
let prefetched: HashMap<String, RepoIndex> = repos
    .par_iter()
    .map(|root| (root.clone(), index_repo(root, ...)))
    .collect();
```

No Mutex boilerplate. rayon handles thread pool + collection.

### 7. Error handling: thiserror in lib, anyhow in bin

Library returns typed errors (`ConfigError`, `CacheError`). Binary uses `anyhow::Result` + `?` for ergonomics. Errors print via `{:#}` for chained context.

## Migration phases (concrete)

**Phase A — Bootstrap**: create `rust/` subdirectory inside the existing repo. Set up Cargo project. Verify `cargo build` succeeds with empty `lib.rs`. Add `.gitignore` for `target/`.

**Phase B — Port leaf modules**:
1. `path.rs` (no deps) — port + 12 tests
2. `exclude.rs` (depends on path) — port + 8 tests
3. `extract.rs` (no lib deps) — port + 60 tests including byte scanners

Validate against Swift binary at each step: feed identical input, diff outputs.

**Phase C — Port middle layer**:
4. `discovery.rs` — uses `walkdir`, depends on `exclude` + `path`
5. `config.rs` — JSON parse + expandPath
6. `cache.rs` — fnv1a64 + cache I/O. Critical compat point: load real Swift-written cache files, verify roundtrip.

**Phase D — Port BFS + CLI**:
7. `crawl.rs` — BFS + CrawlState + applyStyleFixes
8. `main.rs` — clap derive, output rendering, all flags

**Phase E — Validation**:
- Run both binaries on meow-tower, diff orphan + issue + style outputs
- Run both binaries on md-orphan repo (self-check)
- Run both binaries on synthetic test fixtures
- Cache file: write via Swift, read via Rust → identical results
- Performance benchmark: time both on meow-tower

**Phase F — Cutover**:
- Move Rust code to repo root (out of `rust/` subdir)
- Delete Swift sources
- Update `justfile` / `dist/` for `cargo build --release`
- Update README with new install instructions
- Tag a release

## Test strategy

Migrate tests in lockstep with their module. Each Rust module's tests live in `#[cfg(test)] mod tests { ... }` block at the bottom of the file (Rust convention). Integration tests in `tests/integration.rs`.

102 Swift tests → 102 Rust tests. No coverage drop. Parallelize all tests (no global state). Aim for `cargo test` runtime ≤ Swift's current ~50ms.

## Parity validation gates

At each phase, before merging:

1. `cargo test` — all migrated tests pass
2. `cargo clippy -- -D warnings` — clean
3. `cargo fmt --check` — formatted
4. Output diff vs Swift binary on meow-tower → byte-identical for orphans/issues
5. Cache JSON roundtrip Swift↔Rust → byte-identical OR documented schema bump

## Distribution

- macOS arm64 native: `cargo build --release` (target arm64-apple-darwin)
- macOS x86_64 (universal): `cargo build --release --target x86_64-apple-darwin` + `lipo -create`
- Linux musl static: `cross build --release --target x86_64-unknown-linux-musl` (bonus, deferred)
- Existing `dist/md-orphan` symlink → install symlinks to release binary same as before

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Cache JSON shape drift | Custom Deserialize accepting old shape; integration test loading Swift fixture |
| Unicode handling differences (anchor_id) | Tests with Korean/accented/emoji headings; `char::is_alphabetic()` matches Swift `Character.isLetter` for our cases |
| walkdir symlink follow default differs from fts | Use `.follow_links(false)` to match `FTS_PHYSICAL` |
| Scope creep ("rewrite all the things") | No new features during migration; bug-for-bug parity first, then optimize |
| Lost test coverage | Parity validation gate at every phase |
| User has Swift muscle memory | Keep CLI surface (flags, output format, exit codes) byte-identical |

## Non-goals

- No semantic changes to behavior (bug-for-bug parity)
- No new features during migration
- No performance optimization until parity verified
- No cross-platform CI setup (Linux build is bonus, not gating)
- No language-specific idioms that break Swift-era expectations

## Open questions for reviewers

1. **Single-binary repo vs separate Rust workspace**: keep all in one Cargo project, or split into `lib` + `bin` crates? At our scale, single crate is enough.
2. **Should we preserve the Swift code on a branch or delete cleanly?** Argument for keep: rollback path. Argument for delete: less confusion. Recommend: delete after cutover, git history has it.
3. **Should we drop the global `dist/` binary checkin?** Cargo builds locally; pre-built binaries via GitHub Releases.
4. **Tests/MdOrphanTests.swift** is one file; in Rust, split per-module is idiomatic. Loss of single-file overview vs. better cohesion.

## Estimated effort (revised per review)

Adversarial agent's case: 4-5 days is optimistic. Real bites: lifetime annotations, `&str`/`String` boundary churn, error variant proliferation, debug loops on cache JSON shape, `Character.isLetter` ↔ `char::is_alphabetic` Unicode parity, FTS_PHYSICAL ↔ walkdir symlink edge cases.

- Phase A: 1-2 hours (bootstrap)
- Phase B: 1-2 days (leaf modules + tests)
- Phase C: 2-3 days (middle layer + cache schema bump)
- Phase D: 2 days (BFS + CLI)
- Phase E: 1 day (parity validation)
- Phase F: 2-3 hours (cutover + distribution path migration)

**Total: 8-12 working days** realistic for a careful port.

## Reference Round-1 findings

External research (full report in conversation history): clap-derive + thiserror/anyhow + serde + walkdir + rayon. Standard Rust CLI tool stack.

Codebase audit (full report in conversation history): every Swift symbol mapped to Rust equivalent. Largest porting risks: cache JSON compat, Unicode in anchor_id, FTS_PHYSICAL parity.
