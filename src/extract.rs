//! Byte-level link, fence, and heading extraction.
//!
//! Mirrors `Sources/Lib/Extract.swift`. Manual UTF-8 byte scan — Swift Regex was 28-33× slower
//! per [Swift Forums](https://forums.swift.org/t/slow-regex-performance/75768); Rust's regex is
//! fine but for our tight scanners byte slicing is simpler and faster.

use crate::path::{decode_utf8, has_extension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use unicode_segmentation::UnicodeSegmentation;

/// One link extracted from markdown. Carries enough byte-offset info to feed the cache layer
/// and `--fix` byte rewriter without separate "cached" or "detailed" variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub kind: LinkKind,
    pub target: String,
    pub fragment: Option<String>,
    pub path_start: usize,
    pub path_end: usize, // exclusive
}

/// Kind of link, matching the byte-syntax that produced it.
///
/// JSON shape: serde's default external tagging produces `{"wiki":null}`,
/// `{"crossRepo":{"repo":"…"}}` — different from Swift Codable. Cache schema bumped 2→3
/// to invalidate prior Swift-emitted caches (regenerable; one-time miss).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkKind {
    Wiki,
    Standard,
    CrossRepo { repo: String },
}

// MARK: - Link extraction

/// Scan raw UTF-8 bytes for markdown links. `repos` filters cross-repo backtick refs:
/// only `` `path` (name) `` whose `name` is in the set is emitted as `LinkKind::CrossRepo`;
/// non-matching annotations (e.g. `(GridView)`, `(Runtime)`, `(10)`) are ignored as plain
/// inline code.
pub fn extract_links(buf: &[u8], repos: &HashSet<String>) -> Vec<Link> {
    if buf.len() <= 4 {
        return Vec::new();
    }
    let mut links = Vec::new();
    let mut i = 0;
    let mut in_fence = false;
    let count = buf.len();

    while i < count {
        // Code-fence detection at line start (\n or BOF, then ```). Flush-left only.
        let at_line_start = i == 0 || buf[i - 1] == b'\n';
        if at_line_start
            && i + 2 < count
            && buf[i] == b'`'
            && buf[i + 1] == b'`'
            && buf[i + 2] == b'`'
        {
            in_fence = !in_fence;
            while i < count && buf[i] != b'\n' {
                i += 1;
            }
            if i < count {
                i += 1;
            }
            continue;
        }

        if in_fence {
            i += 1;
            continue;
        }

        if i < count - 1 {
            if buf[i] == b'[' && buf[i + 1] == b'[' {
                i = scan_wiki_link(buf, count, i, &mut links);
                continue;
            }
            if buf[i] == b']' && buf[i + 1] == b'(' {
                i = scan_standard_link(buf, count, i, &mut links);
                continue;
            }
        }
        if buf[i] == b'`' {
            i = scan_backtick_ref(buf, count, i, &mut links, repos);
            continue;
        }
        i += 1;
    }

    links
}

/// Decode the fragment after a `#` at `hash_pos`, ending (exclusive) at `end`.
/// `None` when there's no `#` or the fragment is empty (`[[a.md#]]`).
#[inline]
fn parse_fragment(buf: &[u8], hash_pos: Option<usize>, end: usize) -> Option<String> {
    hash_pos.and_then(|h| {
        let frag_len = end - h - 1;
        if frag_len > 0 {
            Some(decode_utf8(buf, h + 1, frag_len))
        } else {
            None
        }
    })
}

// MARK: - Per-syntax scanners

/// Parse `[[page]]`, `[[page|alias]]`, `[[page#section]]` wiki links.
// Index loops are deliberate: scanners track byte offsets (`path_start`/`path_end`) for `--fix`.
#[allow(clippy::needless_range_loop)]
fn scan_wiki_link(buf: &[u8], count: usize, i: usize, links: &mut Vec<Link>) -> usize {
    let start = i + 2;
    let mut end = start;
    while end < count - 1 {
        let b = buf[end];
        if b == b'\n' || b == b'\r' {
            break;
        }
        if b == b']' && buf[end + 1] == b']' {
            break;
        }
        end += 1;
    }
    if !(end < count - 1 && buf[end] == b']' && buf[end + 1] == b']' && end > start) {
        return end + 1;
    }

    let mut name_end = end;
    let mut hash_pos: Option<usize> = None;
    for j in start..end {
        if buf[j] == b'#' {
            hash_pos = Some(j);
            name_end = j;
            break;
        }
        if buf[j] == b'|' {
            name_end = j;
            break;
        }
    }
    let mut frag_end = end;
    if let Some(h) = hash_pos {
        for j in (h + 1)..end {
            if buf[j] == b'|' {
                frag_end = j;
                break;
            }
        }
    }
    let name_len = name_end - start;
    let fragment = parse_fragment(buf, hash_pos, frag_end);
    // Self-anchor link `[[#section]]` / `[[#section|alias]]`: empty path + fragment. Emitted with
    // an empty target so the crawler validates the fragment against the *source* file's headings.
    if name_len == 0 {
        if let Some(fragment) = fragment {
            links.push(Link {
                kind: LinkKind::Wiki,
                target: String::new(),
                fragment: Some(fragment),
                path_start: start,
                path_end: start,
            });
        }
        return end + 2;
    }
    if !has_extension(buf, start, name_len) {
        return end + 2;
    }
    links.push(Link {
        kind: LinkKind::Wiki,
        target: decode_utf8(buf, start, name_len),
        fragment,
        path_start: start,
        path_end: name_end,
    });
    end + 2
}

/// Parse `[text](path.md#fragment)` standard links.
fn scan_standard_link(buf: &[u8], count: usize, i: usize, links: &mut Vec<Link>) -> usize {
    let start = i + 2;
    let mut end = start;
    let mut frag_pos: Option<usize> = None;
    while end < count {
        let b = buf[end];
        if b == b')' || b == b'\n' || b == b'\r' {
            break;
        }
        if b == b'#' && frag_pos.is_none() {
            frag_pos = Some(end);
        }
        end += 1;
    }
    if !(end < count && buf[end] == b')') {
        return end + 1;
    }
    let path_end_byte = frag_pos.unwrap_or(end);
    let path_len = path_end_byte - start;

    // Skip http(s) URLs
    if path_len > 7
        && start + 3 < count
        && buf[start] == b'h'
        && buf[start + 1] == b't'
        && buf[start + 2] == b't'
        && buf[start + 3] == b'p'
    {
        return end + 1;
    }
    if !has_extension(buf, start, path_len) {
        return end + 1;
    }

    let fragment = parse_fragment(buf, frag_pos, end);
    links.push(Link {
        kind: LinkKind::Standard,
        target: decode_utf8(buf, start, path_len),
        fragment,
        path_start: start,
        path_end: path_end_byte,
    });
    end + 1
}

/// Parse `` `path` (repo-name) `` cross-repo backtick refs.
// Index loop is deliberate: tracks byte offsets for `--fix` (see scan_wiki_link).
#[allow(clippy::needless_range_loop)]
fn scan_backtick_ref(
    buf: &[u8],
    count: usize,
    i: usize,
    links: &mut Vec<Link>,
    repos: &HashSet<String>,
) -> usize {
    // Skip multi-backtick spans (``foo``, ```bar```).
    if i + 1 < count && buf[i + 1] == b'`' {
        return i + 1;
    }
    let path_start = i + 1;
    let mut end = path_start;
    while end < count {
        let b = buf[end];
        if b == b'`' {
            break;
        }
        if b == b'\n' || b == b'\r' {
            return i + 1;
        }
        end += 1;
    }
    if !(end < count && buf[end] == b'`' && end > path_start) {
        return i + 1;
    }
    let after_close = end + 1;
    if !(after_close + 2 < count && buf[after_close] == b' ' && buf[after_close + 1] == b'(') {
        return after_close;
    }
    let repo_start = after_close + 2;
    let mut repo_end = repo_start;
    while repo_end < count {
        let rb = buf[repo_end];
        if rb == b')' {
            break;
        }
        if !(rb.is_ascii_alphanumeric() || rb == b'_' || rb == b'-') {
            return after_close;
        }
        repo_end += 1;
    }
    if !(repo_end < count && buf[repo_end] == b')' && repo_end > repo_start) {
        return after_close;
    }
    // Repo-name bytes are validated ASCII alnum/_/- above, so str::from_utf8 is infallible.
    // `contains` accepts a `&str`, so we avoid allocating a `String` for inline-code annotations.
    let repo_str = std::str::from_utf8(&buf[repo_start..repo_end]).unwrap();
    if !repos.contains(repo_str) {
        // `(name)` is a non-repo annotation — plain inline code, not a link.
        return repo_end + 1;
    }
    let mut hash_pos: Option<usize> = None;
    for j in path_start..end {
        if buf[j] == b'#' {
            hash_pos = Some(j);
            break;
        }
    }
    let name_end = hash_pos.unwrap_or(end);
    if !is_valid_cross_repo_path(&buf[path_start..name_end]) {
        return repo_end + 1;
    }
    let fragment = parse_fragment(buf, hash_pos, end);
    links.push(Link {
        kind: LinkKind::CrossRepo { repo: repo_str.to_string() },
        target: decode_utf8(buf, path_start, name_end - path_start),
        fragment,
        path_start,
        path_end: name_end,
    });
    repo_end + 1
}

/// Constraints on a cross-repo backtick path. Annotations like `foo bar.md`,
/// `services/<name>.md`, `./foo.md`, `../foo.md`, `foo.ts` are rejected — they're either
/// prose, docs placeholders, or non-`.md` source refs we don't index.
fn is_valid_cross_repo_path(path: &[u8]) -> bool {
    if !path.ends_with(b".md") || path.len() < 4 {
        return false;
    }
    if path.starts_with(b"./") || path.starts_with(b"../") {
        return false;
    }
    for &b in path {
        if matches!(b, b' ' | b'\t' | b'<' | b'>' | b'{' | b'}') {
            return false;
        }
    }
    true
}

// MARK: - Heading extraction

/// Convert heading text to GitHub-style anchor ID.
/// Lowercase, keep letters/numbers/hyphens/underscores, spaces→hyphens.
///
/// Iterates by **grapheme clusters** (not `char`s) to match Swift `Character`-level iteration.
/// On a decomposed `é` (e + combining acute), Swift treats both as one Character; Rust's `chars()`
/// would split them. `unicode-segmentation`'s `graphemes()` matches Swift's behavior.
pub fn anchor_id(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let lower = text.to_lowercase();
    for g in lower.graphemes(true) {
        if g == " " {
            result.push('-');
            continue;
        }
        // Treat a grapheme as alphanumeric iff its first char is. Matches Swift `Character.isLetter`
        // / `isNumber` (which return true for the cluster's base char's category).
        let first = g.chars().next().unwrap_or('\0');
        if first.is_alphabetic() || first.is_numeric() || first == '-' || first == '_' {
            result.push_str(g);
        }
    }
    result
}

/// Extract heading anchors from markdown content (GitHub-style slugs).
pub fn extract_headings(buf: &[u8]) -> BTreeSet<String> {
    let mut headings = BTreeSet::new();
    let count = buf.len();
    let mut i = 0;

    while i < count {
        if buf[i] == b'#' && (i == 0 || buf[i - 1] == b'\n' || buf[i - 1] == b'\r') {
            let mut j = i;
            while j < count && buf[j] == b'#' {
                j += 1;
            }
            if j < count && buf[j] == b' ' {
                j += 1;
                let head_start = j;
                while j < count && buf[j] != b'\n' && buf[j] != b'\r' {
                    j += 1;
                }
                let mut head_end = j;
                while head_end > head_start
                    && (buf[head_end - 1] == b' ' || buf[head_end - 1] == b'\t')
                {
                    head_end -= 1;
                }
                if head_end > head_start {
                    let text = decode_utf8(buf, head_start, head_end - head_start);
                    headings.insert(anchor_id(&text));
                }
            }
            i = j + 1;
        } else {
            while i < count && buf[i] != b'\n' {
                i += 1;
            }
            i += 1;
        }
    }
    headings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_links_str(s: &str) -> Vec<Link> {
        extract_links(s.as_bytes(), &HashSet::new())
    }

    fn extract_headings_str(s: &str) -> BTreeSet<String> {
        extract_headings(s.as_bytes())
    }

    fn paths(s: &str) -> Vec<String> {
        extract_links_str(s).into_iter().map(|l| l.target).collect()
    }

    fn pairs(s: &str) -> Vec<(String, Option<String>)> {
        extract_links_str(s)
            .into_iter()
            .map(|l| (l.target, l.fragment))
            .collect()
    }

    fn cross_repo_links(s: &str, repos: &[&str]) -> Vec<Link> {
        let set: HashSet<String> = repos.iter().map(|r| (*r).to_string()).collect();
        extract_links(s.as_bytes(), &set)
    }

    // -- known-repo filter (cross-repo backtick refs) --

    #[test]
    fn cross_repo_emitted_when_name_is_known() {
        let links = cross_repo_links("see `foo.md` (meow-tower)", &["meow-tower"]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "foo.md");
        assert_eq!(links[0].kind, LinkKind::CrossRepo { repo: "meow-tower".into() });
    }

    #[test]
    fn cross_repo_filtered_when_name_is_unknown() {
        // `view.name` (GridView) — annotation, not a cross-repo ref. Plain inline code; no link.
        assert!(cross_repo_links("`view.name` (GridView)", &["meow-tower"]).is_empty());
    }

    #[test]
    fn cross_repo_filtered_for_numeric_annotation() {
        assert!(cross_repo_links("`UISortingOrder.Activity` (10)", &["meow-tower"]).is_empty());
    }

    #[test]
    fn cross_repo_filtered_when_repo_set_is_empty() {
        assert!(cross_repo_links("`foo.md` (meow-tower)", &[]).is_empty());
    }

    #[test]
    fn cross_repo_filter_does_not_consume_following_links() {
        // Annotation in front, real wiki link after — wiki link must still be picked up.
        let links = cross_repo_links(
            "`view.name` (GridView) and [[guide.md]]",
            &["meow-tower"],
        );
        let targets: Vec<_> = links.iter().map(|l| l.target.clone()).collect();
        assert_eq!(targets, vec!["guide.md"]);
        assert_eq!(links[0].kind, LinkKind::Wiki);
    }

    #[test]
    fn plain_backtick_span_is_not_a_link() {
        // `path.ext` with no `(repo)` suffix is prose — never a link, even when path-shaped.
        assert!(cross_repo_links("see `foo.md` here", &[]).is_empty());
        assert!(cross_repo_links("see `docs/foo.md#sec`", &[]).is_empty());
    }

    // -- path-shape constraints (only enforced when name is a configured repo) --

    #[test]
    fn cross_repo_filtered_for_non_md_path() {
        // .ts files in cross-repo refs are unverifiable without all-extension indexing — skip.
        assert!(cross_repo_links("`tps-tag-applier.ts` (meow-toolbox)", &["meow-toolbox"]).is_empty());
    }

    #[test]
    fn cross_repo_filtered_for_path_with_whitespace() {
        // `foo bar.md` (meow-tower) — prose with a trailing extension; still not a path.
        assert!(cross_repo_links("`foo bar.md` (meow-tower)", &["meow-tower"]).is_empty());
    }

    #[test]
    fn cross_repo_filtered_for_placeholder_path() {
        // Docs convention: `<name>`, `${name}`, `{name}` mark substitution points.
        assert!(cross_repo_links("`services/<name>.md` (meow-game-server)", &["meow-game-server"]).is_empty());
        assert!(cross_repo_links("`docs/${section}.md` (meow-tower)", &["meow-tower"]).is_empty());
        assert!(cross_repo_links("`{anything}.md` (meow-tower)", &["meow-tower"]).is_empty());
    }

    #[test]
    fn cross_repo_filtered_for_relative_path_prefix() {
        assert!(cross_repo_links("`./foo.md` (meow-tower)", &["meow-tower"]).is_empty());
        assert!(cross_repo_links("`../foo.md` (meow-tower)", &["meow-tower"]).is_empty());
    }

    #[test]
    fn cross_repo_emitted_with_fragment_on_clean_path() {
        let links = cross_repo_links("`docs/foo.md#sec` (meow-tower)", &["meow-tower"]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "docs/foo.md");
        assert_eq!(links[0].fragment.as_deref(), Some("sec"));
    }

    // -- standard links --

    #[test]
    fn extracts_standard_link() {
        assert_eq!(paths("See [guide](docs/guide.md) for details"), vec!["docs/guide.md"]);
    }

    #[test]
    fn extracts_multiple_links() {
        assert_eq!(paths("[a](one.md) text [b](two.md)"), vec!["one.md", "two.md"]);
    }

    #[test]
    fn strips_fragment() {
        assert_eq!(paths("[ref](page.md#section)"), vec!["page.md"]);
    }

    #[test]
    fn handles_relative_dot() {
        assert_eq!(paths("[x](./local.md)"), vec!["./local.md"]);
    }

    #[test]
    fn handles_parent_traversal() {
        assert_eq!(paths("[x](../other/file.md)"), vec!["../other/file.md"]);
    }

    #[test]
    fn skips_http_urls() {
        assert!(paths("[site](https://example.com/page.md)").is_empty());
        assert!(paths("[site](http://example.com/page.md)").is_empty());
    }

    #[test]
    fn extracts_non_markdown_links() {
        assert_eq!(paths("[img](photo.png) [script](app.ts)"), vec!["photo.png", "app.ts"]);
    }

    #[test]
    fn skips_html_tags() {
        assert!(paths("<a href=\"page.md\">link</a>").is_empty());
    }

    #[test]
    fn handles_empty_input() {
        assert!(paths("").is_empty());
    }

    #[test]
    fn handles_no_links() {
        assert!(paths("Just some plain text without any links.").is_empty());
    }

    #[test]
    fn handles_multiple_fragments() {
        assert_eq!(paths("[x](page.md#one#two)"), vec!["page.md"]);
    }

    #[test]
    fn skips_link_spanning_lines() {
        assert!(paths("[text](broken\nlink.md)").is_empty());
    }

    #[test]
    fn handles_adjacent_links() {
        assert_eq!(paths("[a](a.md)[b](b.md)"), vec!["a.md", "b.md"]);
    }

    #[test]
    fn skips_short_paths() {
        assert!(paths("[x](.md)").is_empty());
    }

    // -- wiki links --

    #[test]
    fn extracts_wiki_link() {
        assert_eq!(paths("See [[guide.md]] for details"), vec!["guide.md"]);
    }

    #[test]
    fn extracts_wiki_link_with_alias() {
        assert_eq!(paths("See [[guide.md|the guide]] for details"), vec!["guide.md"]);
    }

    #[test]
    fn extracts_wiki_link_with_fragment() {
        assert_eq!(paths("See [[guide.md#section]] here"), vec!["guide.md"]);
    }

    #[test]
    fn extracts_wiki_link_with_fragment_and_alias() {
        assert_eq!(paths("See [[guide.md#section|display]] here"), vec!["guide.md"]);
    }

    #[test]
    fn extracts_wiki_link_with_path() {
        assert_eq!(paths("See [[docs/guide.md]] for details"), vec!["docs/guide.md"]);
    }

    #[test]
    fn skips_wiki_link_without_extension() {
        assert!(paths("See [[guide]] here").is_empty());
    }

    #[test]
    fn extracts_wiki_link_to_non_md() {
        assert_eq!(paths("See [[image.png]] here"), vec!["image.png"]);
    }

    #[test]
    fn extracts_mixed_links() {
        assert_eq!(paths("[[wiki.md]] and [standard](standard.md)"), vec!["wiki.md", "standard.md"]);
    }

    #[test]
    fn skips_empty_wiki_link() {
        assert!(paths("See [[]] here").is_empty());
    }

    #[test]
    fn skips_wiki_link_spanning_lines() {
        assert!(paths("See [[broken\nlink]] here").is_empty());
    }

    #[test]
    fn extracts_self_anchor_wiki_link() {
        // `[[#section]]` — empty path, fragment present: emitted with empty target.
        let p = pairs("jump to [[#some-section]] above");
        assert_eq!(p, vec![(String::new(), Some("some-section".into()))]);
    }

    #[test]
    fn extracts_self_anchor_wiki_link_with_alias() {
        let p = pairs("jump to [[#some-section|go up]] above");
        assert_eq!(p, vec![(String::new(), Some("some-section".into()))]);
    }

    #[test]
    fn skips_empty_self_anchor_wiki_link() {
        // `[[#]]` — fragment is empty too; nothing to validate.
        assert!(paths("See [[#]] here").is_empty());
    }

    #[test]
    fn extracts_adjacent_wiki_links() {
        assert_eq!(paths("[[one.md]][[two.md]]"), vec!["one.md", "two.md"]);
    }

    // -- fragments --

    #[test]
    fn fragment_from_standard_link() {
        let p = pairs("[ref](page.md#section)");
        assert_eq!(p, vec![("page.md".into(), Some("section".into()))]);
    }

    #[test]
    fn fragment_from_wiki_link() {
        let p = pairs("[[guide.md#core-classes]]");
        assert_eq!(p, vec![("guide.md".into(), Some("core-classes".into()))]);
    }

    #[test]
    fn wiki_link_fragment_with_alias() {
        let p = pairs("[[guide.md#section|display]]");
        assert_eq!(p, vec![("guide.md".into(), Some("section".into()))]);
    }

    #[test]
    fn no_fragment_when_absent() {
        let p = pairs("[ref](page.md)");
        assert_eq!(p, vec![("page.md".into(), None)]);
    }

    #[test]
    fn empty_fragment_treated_as_none() {
        let p = pairs("[ref](page.md#)");
        assert_eq!(p, vec![("page.md".into(), None)]);
    }

    // -- fences + backtick edge cases --

    #[test]
    fn fenced_code_block_skipped() {
        let src = "See [[real.md]]\n\n```\nInside fence: [[fake.md]] and `path.md` (some-repo)\n```\n\nOutside again: [[other.md]]";
        assert_eq!(paths(src), vec!["real.md", "other.md"]);
    }

    #[test]
    fn wiki_link_after_double_backtick_inline_code() {
        let src = "| Inline code | `` `path.ext` `` (no repo) | see [[TODO.md]] |";
        assert_eq!(paths(src), vec!["TODO.md"]);
    }

    // -- headings --

    #[test]
    fn extracts_simple_heading() {
        assert_eq!(extract_headings_str("# Hello World").iter().collect::<Vec<_>>(), vec!["hello-world"]);
    }

    #[test]
    fn extracts_multiple_level_headings() {
        let h = extract_headings_str("# First\n## Second\n### Third");
        let v: Vec<_> = h.iter().cloned().collect();
        // BTreeSet sorts; we just check membership.
        assert!(v.contains(&"first".to_string()));
        assert!(v.contains(&"second".to_string()));
        assert!(v.contains(&"third".to_string()));
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn heading_with_special_chars() {
        let h = extract_headings_str("## Core (Classes)");
        assert!(h.contains("core-classes"));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn heading_with_formatting() {
        let h = extract_headings_str("## **Bold** heading");
        assert!(h.contains("bold-heading"));
    }

    #[test]
    fn heading_with_underscore() {
        let h = extract_headings_str("## hello_world");
        assert!(h.contains("hello_world"));
    }

    #[test]
    fn heading_with_backticks() {
        let h = extract_headings_str("## `code` stuff");
        assert!(h.contains("code-stuff"));
    }

    #[test]
    fn no_headings_in_plain_text() {
        assert!(extract_headings_str("Just text\nMore text").is_empty());
    }

    #[test]
    fn heading_must_have_space_after_hash() {
        assert!(extract_headings_str("#NoSpace").is_empty());
    }

    #[test]
    fn heading_not_mid_line() {
        assert!(extract_headings_str("text ## not a heading").is_empty());
    }

    // -- anchor_id parity with Swift fixture --

    #[test]
    fn anchor_id_parity_with_swift() {
        // Cases captured from the Swift binary; the TSV is the single source (embedded at
        // compile time, so NFC vs NFD café survive byte-exactly). Verifies grapheme-cluster
        // iteration + Unicode category checks match Swift Character semantics.
        let tsv = include_str!("../tests/fixtures/anchor_id_parity.tsv");
        for (n, line) in tsv.lines().enumerate() {
            let (input, expected) = line.split_once('\t').expect("two tab-separated columns");
            let got = anchor_id(input);
            assert_eq!(got, expected, "anchor_id({input:?}) mismatch (tsv line {})", n + 1);
        }
    }
}
