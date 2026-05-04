import Foundation
import Testing
@testable import MdOrphanLib

// MARK: - extractLinks

@Test func extractsStandardLink() {
    let links = extractLinks(from: "See [guide](docs/guide.md) for details").map(\.target)
    #expect(links == ["docs/guide.md"])
}

@Test func extractsMultipleLinks() {
    let links = extractLinks(from: "[a](one.md) text [b](two.md)").map(\.target)
    #expect(links == ["one.md", "two.md"])
}

@Test func stripsFragment() {
    let links = extractLinks(from: "[ref](page.md#section)").map(\.target)
    #expect(links == ["page.md"])
}

@Test func handlesRelativeDot() {
    let links = extractLinks(from: "[x](./local.md)").map(\.target)
    #expect(links == ["./local.md"])
}

@Test func handlesParentTraversal() {
    let links = extractLinks(from: "[x](../other/file.md)").map(\.target)
    #expect(links == ["../other/file.md"])
}

@Test func skipsHttpUrls() {
    let links = extractLinks(from: "[site](https://example.com/page.md)").map(\.target)
    #expect(links.isEmpty)
}

@Test func skipsHttpUrls2() {
    let links = extractLinks(from: "[site](http://example.com/page.md)").map(\.target)
    #expect(links.isEmpty)
}

@Test func extractsNonMarkdownLinks() {
    let links = extractLinks(from: "[img](photo.png) [script](app.ts)").map(\.target)
    #expect(links == ["photo.png", "app.ts"])
}

@Test func skipsHtmlTags() {
    let links = extractLinks(from: "<a href=\"page.md\">link</a>").map(\.target)
    #expect(links.isEmpty)
}

@Test func handlesEmptyInput() {
    let links = extractLinks(from: "").map(\.target)
    #expect(links.isEmpty)
}

@Test func handlesNoLinks() {
    let links = extractLinks(from: "Just some plain text without any links.").map(\.target)
    #expect(links.isEmpty)
}

@Test func handlesMultipleFragments() {
    // Only the first # counts
    let links = extractLinks(from: "[x](page.md#one#two)").map(\.target)
    #expect(links == ["page.md"])
}

@Test func skipsLinkSpanningLines() {
    let links = extractLinks(from: "[text](broken\nlink.md)").map(\.target)
    #expect(links.isEmpty)
}

@Test func handlesAdjacentLinks() {
    let links = extractLinks(from: "[a](a.md)[b](b.md)").map(\.target)
    #expect(links == ["a.md", "b.md"])
}

@Test func skipsShortPaths() {
    // ".md" has no filename before the extension
    let links = extractLinks(from: "[x](.md)").map(\.target)
    #expect(links.isEmpty)
}

// MARK: - extractLinks (wiki links)

@Test func extractsWikiLink() {
    let links = extractLinks(from: "See [[guide.md]] for details").map(\.target)
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithAlias() {
    let links = extractLinks(from: "See [[guide.md|the guide]] for details").map(\.target)
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithFragment() {
    let links = extractLinks(from: "See [[guide.md#section]] here").map(\.target)
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithFragmentAndAlias() {
    let links = extractLinks(from: "See [[guide.md#section|display]] here").map(\.target)
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithPath() {
    let links = extractLinks(from: "See [[docs/guide.md]] for details").map(\.target)
    #expect(links == ["docs/guide.md"])
}

@Test func skipsWikiLinkWithoutExtension() {
    let links = extractLinks(from: "See [[guide]] here").map(\.target)
    #expect(links.isEmpty)
}

@Test func extractsWikiLinkToNonMd() {
    let links = extractLinks(from: "See [[image.png]] here").map(\.target)
    #expect(links == ["image.png"])
}

@Test func extractsMixedLinks() {
    let links = extractLinks(from: "[[wiki.md]] and [standard](standard.md)").map(\.target)
    #expect(links == ["wiki.md", "standard.md"])
}

@Test func skipsEmptyWikiLink() {
    let links = extractLinks(from: "See [[]] here").map(\.target)
    #expect(links.isEmpty)
}

@Test func skipsWikiLinkSpanningLines() {
    let links = extractLinks(from: "See [[broken\nlink]] here").map(\.target)
    #expect(links.isEmpty)
}

@Test func extractsAdjacentWikiLinks() {
    let links = extractLinks(from: "[[one.md]][[two.md]]").map(\.target)
    #expect(links == ["one.md", "two.md"])
}

// MARK: - Link.fragment extraction

private func targetsAndFragments(_ s: String) -> [(String, String?)] {
    extractLinks(from: s).map { ($0.target, $0.fragment) }
}

@Test func fragmentFromStandardLink() {
    let pairs = targetsAndFragments("[ref](page.md#section)")
    #expect(pairs.count == 1 && pairs[0] == ("page.md", "section"))
}

@Test func fragmentFromWikiLink() {
    let pairs = targetsAndFragments("[[guide.md#core-classes]]")
    #expect(pairs.count == 1 && pairs[0] == ("guide.md", "core-classes"))
}

@Test func wikiLinkFragmentWithAlias() {
    let pairs = targetsAndFragments("[[guide.md#section|display]]")
    #expect(pairs.count == 1 && pairs[0] == ("guide.md", "section"))
}

@Test func noFragmentWhenAbsent() {
    let pairs = targetsAndFragments("[ref](page.md)")
    #expect(pairs.count == 1 && pairs[0] == ("page.md", nil))
}

@Test func emptyFragmentTreatedAsNone() {
    let pairs = targetsAndFragments("[ref](page.md#)")
    #expect(pairs.count == 1 && pairs[0] == ("page.md", nil))
}

// MARK: - extractHeadings

@Test func extractsSimpleHeading() {
    let headings = extractHeadings(from: "# Hello World")
    #expect(headings == ["hello-world"])
}

@Test func extractsMultipleLevelHeadings() {
    let headings = extractHeadings(from: "# First\n## Second\n### Third")
    #expect(headings == ["first", "second", "third"])
}

@Test func headingWithSpecialChars() {
    let headings = extractHeadings(from: "## Core (Classes)")
    #expect(headings == ["core-classes"])
}

@Test func headingWithFormatting() {
    let headings = extractHeadings(from: "## **Bold** heading")
    #expect(headings == ["bold-heading"])
}

@Test func headingWithUnderscore() {
    let headings = extractHeadings(from: "## hello_world")
    #expect(headings == ["hello_world"])
}

@Test func headingWithBackticks() {
    let headings = extractHeadings(from: "## `code` stuff")
    #expect(headings == ["code-stuff"])
}

@Test func noHeadingsInPlainText() {
    let headings = extractHeadings(from: "Just text\nMore text")
    #expect(headings.isEmpty)
}

@Test func headingMustHaveSpaceAfterHash() {
    let headings = extractHeadings(from: "#NoSpace")
    #expect(headings.isEmpty)
}

@Test func headingNotMidLine() {
    let headings = extractHeadings(from: "text ## not a heading")
    #expect(headings.isEmpty)
}

// MARK: - resolveLink

@Test func resolvesSimpleLink() {
    let result = resolveLink("guide.md", relativeTo: "/repo/docs/index.md", root: "/repo")
    #expect(result == "/repo/docs/guide.md")
}

@Test func resolvesParentTraversal() {
    let result = resolveLink("../dev/guide.md", relativeTo: "/repo/docs/system/langpack.md", root: "/repo")
    #expect(result == "/repo/docs/dev/guide.md")
}

@Test func resolvesDotSegment() {
    let result = resolveLink("./local.md", relativeTo: "/repo/docs/index.md", root: "/repo")
    #expect(result == "/repo/docs/local.md")
}

@Test func rejectsRootEscape() {
    let result = resolveLink("../../../etc/passwd.md", relativeTo: "/repo/docs/index.md", root: "/repo")
    #expect(result == nil)
}

@Test func resolvesDeepPath() {
    let result = resolveLink("sub/deep/file.md", relativeTo: "/repo/docs/index.md", root: "/repo")
    #expect(result == "/repo/docs/sub/deep/file.md")
}

@Test func resolvesAbsoluteLink() {
    let result = resolveLink("/docs/file.md", relativeTo: "/repo/other/index.md", root: "/repo")
    #expect(result == "/repo/docs/file.md")
}

@Test func normalizesRedundantDots() {
    let result = resolveLink("./a/../b/./c.md", relativeTo: "/repo/docs/index.md", root: "/repo")
    #expect(result == "/repo/docs/b/c.md")
}

// MARK: - dirName

// MARK: - isExcluded

@Test func excludesExactPrefix() {
    #expect(isExcluded("Library/foo/bar.md", by: ["Library"]))
}

@Test func excludesNestedPrefix() {
    #expect(isExcluded("proj-ios/Pods/Firebase/README.md", by: ["proj-ios"]))
}

@Test func doesNotExcludePartialMatch() {
    #expect(!isExcluded("LibraryExtra/file.md", by: ["Library"]))
}

@Test func doesNotExcludeUnrelated() {
    #expect(!isExcluded("docs/guide.md", by: ["Library", "proj-ios"]))
}

@Test func excludesMultiplePatterns() {
    #expect(isExcluded("proj-ios/foo.md", by: ["Library", "proj-ios"]))
    #expect(isExcluded("Library/bar.md", by: ["Library", "proj-ios"]))
}

@Test func excludesFileDirectly() {
    #expect(isExcluded("CHANGELOG.md", by: ["CHANGELOG.md"]))
}

@Test func excludesWithGlob() {
    #expect(isExcluded("docs/draft-intro.md", by: ["docs/draft-*.md"]))
}

@Test func globDoesNotMatchDifferentName() {
    #expect(!isExcluded("docs/guide.md", by: ["docs/draft-*.md"]))
}

@Test func globDoesNotMatchDeeper() {
    #expect(!isExcluded("docs/sub/draft-intro.md", by: ["docs/draft-*.md"]))
}

@Test func globWithQuestionMark() {
    #expect(isExcluded("docs/v1.md", by: ["docs/v?.md"]))
    #expect(!isExcluded("docs/v12.md", by: ["docs/v?.md"]))
}

@Test func trailingSlashAsPrefixPlain() {
    #expect(isExcluded("assets/localizer/cat-profiles/00_Normal.md", by: ["assets/localizer/"]))
    #expect(!isExcluded("assets/other/file.md", by: ["assets/localizer/"]))
}

@Test func trailingSlashAsPrefixGlob() {
    #expect(isExcluded("assets/localizer/cat-profiles/00_Normal.md", by: ["assets/localizer/*/"]))
    #expect(isExcluded("assets/localizer/prompts/deep/file.md", by: ["assets/localizer/*/"]))
    #expect(!isExcluded("assets/localizer/top.md", by: ["assets/localizer/*/"]))
}

@Test func mixesPrefixAndGlob() {
    #expect(isExcluded("Library/foo.md", by: ["Library", "docs/draft-*.md"]))
    #expect(isExcluded("docs/draft-intro.md", by: ["Library", "docs/draft-*.md"]))
    #expect(!isExcluded("docs/guide.md", by: ["Library", "docs/draft-*.md"]))
}

// MARK: - bfsCrawl
// Serialized because bfsCrawl/readFile share a global read buffer.

private func withTempDir(_ body: (String) -> Void) {
    let dir = NSTemporaryDirectory() + "md-orphan-test-\(UUID().uuidString)"
    mkdir(dir, 0o755)
    defer { try? FileManager.default.removeItem(atPath: dir) }
    body(dir)
}

private func writeFile(_ path: String, _ content: String) {
    content.withCString { ptr in
        let fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
        write(fd, ptr, strlen(ptr))
        close(fd)
    }
}

@Suite(.serialized) struct BfsCrawlTests {
    @Test func findsBrokenLinks() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[a](missing.md)")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.count == 1)
            #expect(issues[0].link == "missing.md")
            #expect(issues[0].kind == .broken)
        }
    }

    @Test func noBrokenOnValidLinks() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[a](other.md)")
            writeFile("\(root)/other.md", "hello")
            let (_, reachable, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.isEmpty)
            #expect(reachable.count == 2)
        }
    }

    @Test func basenameFallback() {
        withTempDir { root in
            mkdir("\(root)/docs", 0o755)
            writeFile("\(root)/index.md", "[a](guide.md)")
            writeFile("\(root)/docs/guide.md", "hello")
            let (_, reachable, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.isEmpty)
            #expect(reachable.count == 2)
        }
    }

    @Test func ambiguousLink() {
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            writeFile("\(root)/index.md", "[a](guide.md)")
            writeFile("\(root)/a/guide.md", "hello")
            writeFile("\(root)/b/guide.md", "hello")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.count == 1)
            #expect(issues[0].link == "guide.md")
            if case .ambiguous(let count) = issues[0].kind {
                #expect(count == 2)
            } else {
                Issue.record("Expected ambiguous, got \(issues[0].kind)")
            }
        }
    }

    @Test func brokenNonMdLink() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[img](photo.png)")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.count == 1)
            #expect(issues[0].link == "photo.png")
            #expect(issues[0].kind == .broken)
        }
    }

    @Test func validNonMdLink() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[img](photo.png)")
            writeFile("\(root)/photo.png", "fake image data")
            let (_, reachable, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.isEmpty)
            // Non-.md files should not be in reachable set (only .md files are crawled)
            #expect(reachable.count == 1)
        }
    }

    @Test func brokenAnchor() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[ref](other.md#missing-section)")
            writeFile("\(root)/other.md", "# Existing Section\n\nSome content")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.count == 1)
            if case .brokenAnchor(let frag) = issues[0].kind {
                #expect(frag == "missing-section")
            } else {
                Issue.record("Expected brokenAnchor, got \(issues[0].kind)")
            }
        }
    }

    @Test func validAnchor() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[ref](other.md#existing-section)")
            writeFile("\(root)/other.md", "# Existing Section\n\nSome content")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.isEmpty)
        }
    }

    @Test func wikiLinkBrokenAnchor() {
        withTempDir { root in
            writeFile("\(root)/index.md", "[[other.md#no-such-heading]]")
            writeFile("\(root)/other.md", "# Real Heading\n\nContent")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            #expect(issues.count == 1)
            if case .brokenAnchor(let frag) = issues[0].kind {
                #expect(frag == "no-such-heading")
            } else {
                Issue.record("Expected brokenAnchor, got \(issues[0].kind)")
            }
        }
    }

    @Test func wikiStyleRelativePathFlagged() {
        withTempDir { root in
            mkdir("\(root)/docs", 0o755)
            mkdir("\(root)/docs/dev", 0o755)
            mkdir("\(root)/docs/system", 0o755)
            writeFile("\(root)/docs/dev/index.md", "see [[../system/foo.md]]")
            writeFile("\(root)/docs/system/foo.md", "hi")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/docs/dev/index.md"], root: root)
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.count == 1)
            if case .style(_, let suggested, _, _) = style[0].kind {
                #expect(suggested == "foo.md")
            } else {
                Issue.record("Expected style, got \(style[0].kind)")
            }
        }
    }

    @Test func wikiStyleRedundantRepoPathFlagged() {
        withTempDir { root in
            mkdir("\(root)/docs", 0o755)
            writeFile("\(root)/index.md", "see [[docs/foo.md]]")
            writeFile("\(root)/docs/foo.md", "hi")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.count == 1)
            if case .style(_, let suggested, _, _) = style[0].kind {
                #expect(suggested == "foo.md")
            }
        }
    }

    @Test func wikiStyleDuplicateBasenameKeepsPath() {
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            writeFile("\(root)/index.md", "see [[a/foo.md]] and [[b/foo.md]]")
            writeFile("\(root)/a/foo.md", "x")
            writeFile("\(root)/b/foo.md", "y")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root)
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.isEmpty)
        }
    }

    @Test func wikiStyleAlreadyCanonicalNotFlagged() {
        withTempDir { root in
            mkdir("\(root)/docs", 0o755)
            writeFile("\(root)/docs/index.md", "see [[foo.md]]")
            writeFile("\(root)/docs/foo.md", "hi")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/docs/index.md"], root: root)
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.isEmpty)
        }
    }

    @Test func wikiStylePreservesFragmentAndAlias() {
        withTempDir { root in
            mkdir("\(root)/docs", 0o755)
            mkdir("\(root)/docs/dev", 0o755)
            mkdir("\(root)/docs/system", 0o755)
            writeFile("\(root)/docs/dev/index.md", "see [[../system/foo.md#bar|alias]]")
            writeFile("\(root)/docs/system/foo.md", "# Bar\n")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/docs/dev/index.md"], root: root)
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.count == 1)
            if case .style(_, let suggested, let s, let e) = style[0].kind {
                #expect(suggested == "foo.md")
                // The replaced range covers only `../system/foo.md`, not the fragment or alias
                let content = "see [[../system/foo.md#bar|alias]]"
                let bytes = Array(content.utf8)
                let slice = String(decoding: bytes[s..<e], as: UTF8.self)
                #expect(slice == "../system/foo.md")
            }
        }
    }

    @Test func wikiStyleStandardLinkNotFlagged() {
        // Style rule applies only to wiki [[...]] links, not [text](...) standard links.
        withTempDir { root in
            mkdir("\(root)/docs", 0o755)
            mkdir("\(root)/docs/dev", 0o755)
            mkdir("\(root)/docs/system", 0o755)
            writeFile("\(root)/docs/dev/index.md", "[x](../system/foo.md)")
            writeFile("\(root)/docs/system/foo.md", "hi")
            let (_, _, issues) = bfsCrawl(entryPaths: ["\(root)/docs/dev/index.md"], root: root)
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.isEmpty)
        }
    }

    // MARK: - cross-repo crawl

    @Test func crossRepoUnknownRepoFlagged() {
        withTempDir { root in
            writeFile("\(root)/index.md", "see `foo.md` (no-such-repo)")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/index.md"], root: root,
                options: CrawlOptions(repos: [:])
            )
            let unknowns = issues.filter { if case .unknownRepo = $0.kind { return true }; return false }
            #expect(unknowns.count == 1)
            if case .unknownRepo(let repo) = unknowns[0].kind {
                #expect(repo == "no-such-repo")
            }
        }
    }

    @Test func crossRepoBrokenLinkFlagged() {
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            writeFile("\(root)/a/index.md", "see `missing.md` (b)")
            writeFile("\(root)/b/other.md", "")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/a/index.md"], root: "\(root)/a",
                options: CrawlOptions(repos: ["b": "\(root)/b"])
            )
            let broken = issues.filter { if case .crossRepoBroken = $0.kind { return true }; return false }
            #expect(broken.count == 1)
            if case .crossRepoBroken(let r) = broken[0].kind {
                #expect(r == "b")
            }
        }
    }

    @Test func crossRepoStyleViolationSuggestsBasename() {
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            mkdir("\(root)/b/docs", 0o755)
            writeFile("\(root)/a/index.md", "see `docs/foo.md` (b)")
            writeFile("\(root)/b/docs/foo.md", "# Foo")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/a/index.md"], root: "\(root)/a",
                options: CrawlOptions(repos: ["b": "\(root)/b"])
            )
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.count == 1)
            if case .style(let scope, let suggested, _, _) = style[0].kind {
                #expect(suggested == "foo.md")
                if case .crossRepo(let r) = scope { #expect(r == "b") } else {
                    Issue.record("expected crossRepo scope")
                }
            }
        }
    }

    @Test func crossRepoEscapingPathFallsBackToBasename() {
        // `../docs/foo.md` (b) escapes the b root; basename fallback resolves it,
        // and style suggestion is the canonical basename.
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            mkdir("\(root)/b/docs", 0o755)
            writeFile("\(root)/a/index.md", "see `../docs/foo.md` (b)")
            writeFile("\(root)/b/docs/foo.md", "")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/a/index.md"], root: "\(root)/a",
                options: CrawlOptions(repos: ["b": "\(root)/b"])
            )
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.count == 1)
            if case .style(_, let suggested, _, _) = style[0].kind {
                #expect(suggested == "foo.md")
            }
        }
    }

    @Test func crossRepoCanonicalNotFlagged() {
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            writeFile("\(root)/a/index.md", "see `foo.md` (b)")
            writeFile("\(root)/b/foo.md", "")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/a/index.md"], root: "\(root)/a",
                options: CrawlOptions(repos: ["b": "\(root)/b"])
            )
            let style = issues.filter { if case .style = $0.kind { return true }; return false }
            #expect(style.isEmpty)
        }
    }

    @Test func crossRepoFollowedRecursivelyForBrokenLinkInTarget() {
        // A → cross-repo B/page.md → references B/missing.md (broken).
        // The broken link inside the cross-repo target should be reported.
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            writeFile("\(root)/a/index.md", "see `page.md` (b)")
            writeFile("\(root)/b/page.md", "[here](missing.md)")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/a/index.md"], root: "\(root)/a",
                options: CrawlOptions(repos: ["b": "\(root)/b"])
            )
            let broken = issues.filter { $0.kind == .broken }
            #expect(broken.count == 1)
            #expect(broken[0].link == "missing.md")
        }
    }

    @Test func crossRepoFragmentValidatedAgainstTargetHeadings() {
        withTempDir { root in
            mkdir("\(root)/a", 0o755)
            mkdir("\(root)/b", 0o755)
            writeFile("\(root)/a/index.md", "see `foo.md#missing-section` (b)")
            writeFile("\(root)/b/foo.md", "# Real Heading")
            let (_, _, issues) = bfsCrawl(
                entryPaths: ["\(root)/a/index.md"], root: "\(root)/a",
                options: CrawlOptions(repos: ["b": "\(root)/b"])
            )
            let anchors = issues.filter { if case .brokenAnchor = $0.kind { return true }; return false }
            #expect(anchors.count == 1)
            if case .brokenAnchor(let frag) = anchors[0].kind {
                #expect(frag == "missing-section")
            }
        }
    }

    // MARK: - fenced code block skipping

    @Test func fencedCodeBlockSkipped() {
        let src = """
        See [[real.md]]

        ```
        Inside fence: [[fake.md]] and `path.md` (some-repo)
        ```

        Outside again: [[other.md]]
        """
        let paths = extractLinks(from: src).map(\.target)
        #expect(paths == ["real.md", "other.md"])
    }

    @Test func wikiLinkAfterDoubleBacktickInlineCode() {
        // Regression: a row mixing double-backtick code spans and a wiki link.
        // The backtick scanner must not eat past the [[ marker.
        let src = "| Inline code | `` `path.ext` `` (no repo) | see [[TODO.md]] |"
        let paths = extractLinks(from: src).map(\.target)
        #expect(paths == ["TODO.md"])
    }
}

// MARK: - config + ignore

@Test func expandPathHomeAndEnv() throws {
    setenv("MD_ORPHAN_TEST_X", "/tmp/something", 1)
    defer { unsetenv("MD_ORPHAN_TEST_X") }
    let result = try expandPath("$MD_ORPHAN_TEST_X/sub", repoName: "test")
    #expect(result == "/tmp/something/sub")
    let braced = try expandPath("${MD_ORPHAN_TEST_X}/sub", repoName: "test")
    #expect(braced == "/tmp/something/sub")
}

@Test func expandPathMissingVarThrows() {
    let didThrow: Bool
    do {
        _ = try expandPath("$NOPE_NOT_DEFINED_VAR/x", repoName: "r")
        didThrow = false
    } catch { didThrow = true }
    #expect(didThrow)
}

@Test func loadProjectIgnoreParsesLines() throws {
    let dir = NSTemporaryDirectory() + "md-orphan-ignore-\(UUID().uuidString)"
    mkdir(dir, 0o755)
    defer { try? FileManager.default.removeItem(atPath: dir) }
    let path = dir + "/.md-orphan"
    try """
    # comment
    Library/

    docs/draft-*.md
    """.write(toFile: path, atomically: true, encoding: .utf8)
    let patterns = try loadProjectIgnore(root: dir)
    #expect(patterns == ["Library/", "docs/draft-*.md"])
}

@Test func loadProjectIgnoreReturnsEmptyWhenAbsent() throws {
    let dir = NSTemporaryDirectory() + "md-orphan-ignore-\(UUID().uuidString)"
    mkdir(dir, 0o755)
    defer { try? FileManager.default.removeItem(atPath: dir) }
    let patterns = try loadProjectIgnore(root: dir)
    #expect(patterns.isEmpty)
}

// MARK: - cache
// Cache tests serialized with BfsCrawlTests because ExtractionCache.read uses readFile's global buffer.

extension BfsCrawlTests {
@Test func extractionCacheSerializesAndRoundtrips() {
    let dir = NSTemporaryDirectory() + "md-orphan-cache-\(UUID().uuidString)"
    mkdir(dir, 0o755)
    defer { try? FileManager.default.removeItem(atPath: dir) }

    let content = "# Title\n\nSee [[other.md]] and [[third.md#sec]]"
    let path = dir + "/index.md"
    try? content.write(toFile: path, atomically: true, encoding: .utf8)

    let canonicalDir = realPath(dir)!
    let canonicalPath = canonicalDir + "/index.md"

    let cache = ExtractionCache(enabled: true)
    guard let first = cache.read(filePath: canonicalPath, repoRoot: canonicalDir) else {
        Issue.record("first read failed"); return
    }
    #expect(first.links.count == 2)
    #expect(first.headings.contains("title"))

    // Second read with same content → cache hit returns identical structured output.
    guard let second = cache.read(filePath: canonicalPath, repoRoot: canonicalDir) else {
        Issue.record("second read failed"); return
    }
    #expect(second.links == first.links)
    #expect(second.headings == first.headings)
}

@Test func extractionCacheInvalidatesOnContentChange() {
    let dir = NSTemporaryDirectory() + "md-orphan-cache-inv-\(UUID().uuidString)"
    mkdir(dir, 0o755)
    defer { try? FileManager.default.removeItem(atPath: dir) }

    let path = dir + "/index.md"
    try? "[[a.md]]".write(toFile: path, atomically: true, encoding: .utf8)

    let canonicalDir = realPath(dir)!
    let canonicalPath = canonicalDir + "/index.md"

    let cache = ExtractionCache(enabled: true)
    _ = cache.read(filePath: canonicalPath, repoRoot: canonicalDir)

    // Sleep briefly so mtime differs even on coarse FS, then change content + size.
    usleep(20_000)
    try? "[[b.md]] and [[c.md]]".write(toFile: path, atomically: true, encoding: .utf8)

    guard let result = cache.read(filePath: canonicalPath, repoRoot: canonicalDir) else {
        Issue.record("read after change failed"); return
    }
    #expect(result.links.map(\.target) == ["b.md", "c.md"])
}

@Test func cacheFilePathHashesCanonicalRoot() {
    let p1 = cacheFilePath(forCanonicalRoot: "/Users/jk/proj-a")
    let p2 = cacheFilePath(forCanonicalRoot: "/Users/jk/proj-b")
    #expect(p1 != p2)
    #expect(p1.hasSuffix(".json"))
    #expect(p1.contains("/cache/"))
}

@Test func fnv1a64Stability() {
    // Spot-check determinism — same input → same hash.
    #expect(fnv1a64Hex("hello") == fnv1a64Hex("hello"))
    #expect(fnv1a64Hex("hello") != fnv1a64Hex("hellp"))
}
}

// MARK: - dirName

@Test func dirNameOfFilePath() {
    #expect(dirName("/repo/docs/file.md") == "/repo/docs")
}

@Test func dirNameOfRootFile() {
    #expect(dirName("/file.md") == "")
}

@Test func dirNameNoSlash() {
    #expect(dirName("file.md") == ".")
}

// MARK: - benchmarks (skipped by default)

@Test(.disabled("manual benchmark — temporarily un-disable and `swift test -c release --filter benchmarkMeowTower` to run"))
func benchmarkMeowTower() {
    let root = "/Users/jameskim/Develop/meow-tower"

    var t0 = Date()
    let idx = indexRepo(root: root, exclude: [], useDefaultExcludes: true)
    print("indexRepo (defaults): \(String(format: "%.3f", Date().timeIntervalSince(t0)))s | md=\(idx.mdFiles.count) | basenames=\(idx.byName.count) | total=\(idx.byName.values.reduce(0) { $0 + $1.count })")

    t0 = Date()
    let idx2 = indexRepo(root: root, exclude: [], useDefaultExcludes: false)
    print("indexRepo (no defaults): \(String(format: "%.3f", Date().timeIntervalSince(t0)))s | total=\(idx2.byName.values.reduce(0) { $0 + $1.count })")

    t0 = Date()
    var totalLinks = 0
    var totalHeadings = 0
    for path in idx.mdFiles.values {
        let abs = idx.root + "/" + path
        guard let (_, buf) = readFile(path: abs) else { continue }
        totalLinks += extractLinks(buf).count
        totalHeadings += extractHeadings(buf).count
    }
    print("read+extract \(idx.mdFiles.count) md: \(String(format: "%.3f", Date().timeIntervalSince(t0)))s | links=\(totalLinks) headings=\(totalHeadings)")
}
