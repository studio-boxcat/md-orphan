//! md-orphan — detect orphan markdown files + broken/style-violating links.
//!
//! See [[architecture.md]] for module layout and design rationale.

pub mod cache;
pub mod config;
pub mod crawl;
pub mod discovery;
pub mod exclude;
pub mod extract;
pub mod path;
pub mod walk_cache;
