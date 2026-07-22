> **Related:** [[architecture.md]], [[performance.md]], [[CLAUDE.md]]

# Swift→Rust migration record

> **Historical document.** This is the *pre-implementation plan* for the 2026 Swift→Rust port, kept for the rationale below — not a description of the code. Several of its choices were reversed during implementation: the walker is `ignore::WalkParallel`, not `walkdir`; parallelism is `std::thread::scope`, not `rayon`; `thiserror` (slated to be dropped in favor of `anyhow`-only) shipped as a live dependency; and tests live in per-module `#[cfg(test)]` blocks rather than a monolithic `tests/integration.rs`. For what actually shipped, see [[architecture.md]].

## Why Rust

Honest assessment, not perf-justified alone:

- **Cross-platform**: opens Linux CI runners, Windows-bonus
- **Smaller binary**: ~5MB Swift → ~2MB Rust
- **Better walker ecosystem**: best-in-class walker crates (fd/ripgrep lineage)
- **Simpler concurrency**: scoped threads vs `DispatchQueue.concurrentPerform`
- **Tighter byte-slice paths**: no `String(cString:)` allocations per fts entry
- **Cargo + crates.io**: better dependency story than SwiftPM at this scale

What it **doesn't** buy once the walk-cache lands: cold-walk speed (a warm cache is ~equally fast regardless of language).

## Decisions that still explain shipped behavior

### JSON cache: schema bump 2→3, no compat dance

Swift `Codable` for `case crossRepo(repo: String)` emits externally-tagged JSON (`{"crossRepo":{"repo":"…"}}`); serde's tagged-enum representations emit different shapes. They are not interoperable, and a custom Deserialize accepting both would be fragile and untestable in the long tail. Since the cache is regenerable, the schema version was bumped 2→3 instead — Swift-era caches silently invalidate on first run, a one-time harmless miss.

### fnv1a64 byte-compat

Swift `String(h, radix: 16)` and Rust `format!("{:x}", h)` both produce lowercase hex with no leading zeros, and FNV-1a constants are deterministic — so cache file *names* (hex hash of the canonical root) survived the migration byte-identical.

### anchor_id Unicode parity via captured fixture

The Swift slugger leaned on `Character.isLetter` (grapheme-cluster semantics); Korean/accented/emoji headings had to slug identically in Rust or existing anchors would silently break. Rather than argue `char::is_alphabetic()` equivalence by inspection, the Swift binary's slugger output was captured as a TSV fixture — `tests/fixtures/anchor_id_parity.tsv` — and the Rust grapheme-aware implementation is tested against it. Parity by construction, not by review.
