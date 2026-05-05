//! md-orphan — detect orphan markdown files + broken/style-violating links.
//!
//! See `docs/architecture.md` (in the repo root) for module layout and design rationale.

pub mod exclude;
pub mod extract;
pub mod path;
