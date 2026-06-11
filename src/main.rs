//! md-orphan CLI — Rust port of `Sources/CLI/main.swift`. Mirrors flags + output format
//! byte-identically with the Swift binary so existing lefthook hooks + scripts continue working.

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use md_orphan::cache::ExtractionCache;
use md_orphan::config::{
    default_config_path, find_project_ignore_ancestor, load_global_config, project_ignore_exists,
};
use md_orphan::crawl::{
    apply_style_fixes, bfs_crawl_at_root, CrawlOptions, IssueKind, LinkIssue, StyleScope,
};
use md_orphan::path::{base_name, dir_name, is_under, real_path, rel_path};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "md-orphan",
    about = "Detect markdown files not reachable from entry points"
)]
struct Cli {
    /// One or more markdown entry points
    entry_points: Vec<String>,

    /// Exclude paths by prefix or glob; * and ? don't cross / (comma-separated, repeatable)
    #[arg(long)]
    exclude: Vec<String>,

    /// Show success message when all files are reachable
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Rewrite link style issues in place
    #[arg(long)]
    fix: bool,

    /// Path to global config (default: $XDG_CONFIG_HOME/md-orphan/md-orphan.json)
    #[arg(long)]
    config: Option<String>,

    /// Disable built-in default excludes (.git, node_modules, Library, .build, ...)
    #[arg(long)]
    no_default_excludes: bool,

    /// Disable per-file extraction cache
    #[arg(long)]
    no_cache: bool,

    /// Index every file extension so non-.md refs ([[foo.cs]], `src/foo.cs`) get style and
    /// basename resolution (~30x walk cost on large repos)
    #[arg(long)]
    all_extensions: bool,

    /// Print md-orphan's own CLAUDE.md (usage guide for this tool)
    #[arg(long)]
    orient: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("md-orphan: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<bool> {
    if cli.orient {
        return run_orient();
    }
    if cli.entry_points.is_empty() {
        bail!("missing entry point");
    }

    let resolved_entries: Vec<String> = cli
        .entry_points
        .iter()
        .map(|ep| {
            real_path(ep)
                .map(|p| p.to_string_lossy().to_string())
                .ok_or_else(|| anyhow!("{ep}: no such file"))
        })
        .collect::<Result<Vec<_>>>()?;

    let exclude_patterns: Vec<String> = cli
        .exclude
        .iter()
        .flat_map(|s| s.split(',').map(|x| x.to_string()))
        .collect();
    for p in &exclude_patterns {
        if p.contains("**") {
            bail!("--exclude: '**' globstar not supported; * matches within a single directory");
        }
        if p.contains('[') && !p.contains(']') {
            bail!("--exclude: unclosed '[' in pattern '{p}'");
        }
    }

    let config_path = cli
        .config
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let mut global_config = load_global_config(&config_path).with_context(|| "loading global config")?;
    // Treat configured-but-not-cloned repos as if they weren't configured at all — refs to them
    // become inline-code annotations rather than spurious broken-link errors.
    global_config.retain_existing();

    let root = dir_name(&resolved_entries[0]).to_string();
    // The first entry's parent defines the repo root. An entry outside it (sibling dir)
    // would be crawled against the wrong root — broken-link-prone — so reject up front.
    for ep in &resolved_entries[1..] {
        if !is_under(ep, &root) {
            bail!(
                "{ep}: outside repo root `{root}` (derived from the first entry point)\n\
                 All entry points must live under one repo root; run md-orphan once per repo."
            );
        }
    }
    let root_path = std::path::Path::new(&root);
    if !project_ignore_exists(root_path) {
        if let Some(ancestor) = find_project_ignore_ancestor(root_path) {
            let ancestor_str = ancestor.to_string_lossy();
            let example = ["CLAUDE.md", "README.md"]
                .iter()
                .find(|name| ancestor.join(name).exists())
                .map(|name| format!(" (e.g. `md-orphan {ancestor_str}/{name}`)"))
                .unwrap_or_default();
            bail!(
                "{root}/.md-orphan: missing, but an ancestor has one:\n  {ancestor_str}/.md-orphan\n\n\
md-orphan treats the entry point's parent as the repo root. Your entry point is below the\n\
configured root — pass an entry point inside `{ancestor_str}` instead{example}.\n\n\
If `{root}` really is a separate repo with its own scope, create `{root}/.md-orphan` to declare it."
            );
        }
        bail!(
            "{root}/.md-orphan: missing\nEvery md-orphan-checked repo needs a `.md-orphan` file at its root listing project-specific\n\
ignore patterns (gitignore-style line patterns). Built-in defaults handle .git, node_modules,\n\
Library, Pods, etc., but project-specific dirs (Unity Packages, vendored docs, build outputs)\n\
must be enumerated explicitly. Create the file with `touch {root}/.md-orphan` if you have\n\
no extras."
        );
    }

    let crawl_options = CrawlOptions {
        repos: global_config.repos,
        use_default_excludes: !cli.no_default_excludes,
        extra_excludes: exclude_patterns,
        use_walk_cache: !cli.no_cache,
        include_all_extensions: cli.all_extensions,
    };
    let mut cache = ExtractionCache::new(
        !cli.no_cache,
        crawl_options.repos.keys().cloned().collect(),
    );
    let (entry_index, reachable, issues) = bfs_crawl_at_root(
        &resolved_entries,
        &root,
        &crawl_options,
        &mut cache,
    );
    // Persist the per-file extraction cache (the walk-cache saves itself inside index_repo).
    // Files rewritten by --fix below self-invalidate next run via (mtime, size, content-hash).
    cache.save();

    let entry_root_str = entry_index.root.as_str();
    let orphans: Vec<String> = {
        let mut v: Vec<String> = entry_index
            .md_files
            .iter()
            .filter(|rel| {
                let abs = format!("{}/{}", entry_root_str, rel);
                if reachable.contains(abs.as_str()) {
                    return false;
                }
                if let Some(canonical) = real_path(&abs)
                    && reachable.contains(canonical.to_string_lossy().as_ref()) {
                        return false;
                    }
                true
            })
            .cloned()
            .collect();
        v.sort();
        v
    };

    let names = cli
        .entry_points
        .iter()
        .map(|p| base_name(p).to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let failed = render_issues(&issues, &orphans, &root, &names, cli.fix);
    if !failed && cli.verbose {
        println!(
            "\u{2705} All {} markdown files are reachable from {names}",
            entry_index.md_files.len()
        );
    }
    Ok(!failed)
}

/// Print every issue group + orphans (stdout), apply `--fix` when asked, and return whether
/// the run failed. Output format byte-matches the Swift-era binary for existing kinds.
fn render_issues(
    issues: &[LinkIssue],
    orphans: &[String],
    root: &str,
    names: &str,
    fix: bool,
) -> bool {
    let rel_source = |issue: &LinkIssue| {
        rel_path(&issue.source, root).map(|s| s.to_string()).unwrap_or_else(|| issue.source.clone())
    };

    let mut failed = false;

    let broken: Vec<&LinkIssue> = issues.iter().filter(|i| matches!(i.kind, IssueKind::Broken)).collect();
    let ambiguous: Vec<&LinkIssue> = issues.iter().filter(|i| matches!(i.kind, IssueKind::Ambiguous(_))).collect();
    let broken_anchors: Vec<&LinkIssue> = issues.iter().filter(|i| matches!(i.kind, IssueKind::BrokenAnchor(_))).collect();
    let cross_repo_broken: Vec<&LinkIssue> = issues.iter().filter(|i| matches!(i.kind, IssueKind::CrossRepoBroken(_))).collect();
    let style_issues: Vec<&LinkIssue> = issues.iter().filter(|i| matches!(i.kind, IssueKind::Style { .. })).collect();
    let unreadable: Vec<&LinkIssue> = issues.iter().filter(|i| matches!(i.kind, IssueKind::Unreadable)).collect();

    if !unreadable.is_empty() {
        println!("\u{26D4} {} unreadable files:", unreadable.len());
        for u in &unreadable {
            println!("  {}", rel_source(u));
        }
        failed = true;
    }

    if !broken.is_empty() {
        println!("\u{1F517} {} broken links:", broken.len());
        for b in &broken {
            println!("  {} in {}", b.link, rel_source(b));
        }
        failed = true;
    }
    if !ambiguous.is_empty() {
        println!("\u{26A0}\u{FE0F} {} ambiguous links:", ambiguous.len());
        for a in &ambiguous {
            if let IssueKind::Ambiguous(count) = a.kind {
                println!("  {} in {} ({count} files match)", a.link, rel_source(a));
            }
        }
        failed = true;
    }
    if !broken_anchors.is_empty() {
        println!("\u{2693} {} broken anchors:", broken_anchors.len());
        for a in &broken_anchors {
            if let IssueKind::BrokenAnchor(frag) = &a.kind {
                println!("  {}#{} in {}", a.link, frag, rel_source(a));
            }
        }
        failed = true;
    }
    if !cross_repo_broken.is_empty() {
        println!("\u{1F517} {} broken cross-repo links:", cross_repo_broken.len());
        for b in &cross_repo_broken {
            if let IssueKind::CrossRepoBroken(r) = &b.kind {
                println!("  `{}` ({r}) in {}", b.link, rel_source(b));
            }
        }
        failed = true;
    }
    if !style_issues.is_empty() {
        let header = if fix { "fixed" } else { "issues" };
        println!("\u{1F4DD} {} link style {header}:", style_issues.len());
        let mut sorted: Vec<&LinkIssue> = style_issues.clone();
        sorted.sort_by(|a, b| {
            let asrc = rel_source(a);
            let bsrc = rel_source(b);
            if asrc != bsrc {
                return asrc.cmp(&bsrc);
            }
            let ai = match &a.kind {
                IssueKind::Style { path_start, .. } => *path_start,
                _ => 0,
            };
            let bi = match &b.kind {
                IssueKind::Style { path_start, .. } => *path_start,
                _ => 0,
            };
            ai.cmp(&bi)
        });
        for issue in &sorted {
            if let IssueKind::Style { scope, suggested, .. } = &issue.kind {
                let (lhs, rhs) = render_style(&issue.link, suggested, scope);
                println!("  {}: {lhs} -> {rhs}", rel_source(issue));
            }
        }
        if fix {
            let owned: Vec<LinkIssue> = style_issues.iter().map(|i| (*i).clone()).collect();
            if apply_style_fixes(&owned) > 0 {
                failed = true; // some reported fixes did not land; stderr has the details
            }
        } else {
            println!("  (run with --fix to apply)");
            failed = true;
        }
    }

    if !orphans.is_empty() {
        println!(
            "\u{274C} {} orphan markdown files (not reachable from {names}):",
            orphans.len()
        );
        for path in orphans {
            println!("  {path}");
        }
        failed = true;
    }

    failed
}

fn render_style(link: &str, suggested: &str, scope: &StyleScope) -> (String, String) {
    match scope {
        StyleScope::Wiki => (format!("[[{link}]]"), format!("[[{suggested}]]")),
        StyleScope::CrossRepo { repo } => (
            format!("`{link}` ({repo})"),
            format!("`{suggested}` ({repo})"),
        ),
        StyleScope::Inline => (format!("`{link}`"), format!("`{suggested}`")),
    }
}

/// Print md-orphan's own CLAUDE.md, embedded at compile time so the binary is self-contained.
fn run_orient() -> Result<bool> {
    const GUIDE: &str = include_str!("../CLAUDE.md");
    if GUIDE.ends_with('\n') {
        print!("{GUIDE}");
    } else {
        println!("{GUIDE}");
    }
    Ok(true)
}
