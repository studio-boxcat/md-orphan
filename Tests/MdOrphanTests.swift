import Foundation
import Testing
@testable import MdOrphanLib

// MARK: - extractLinks

@Test func extractsStandardLink() {
    let links = extractLinks(from: "See [guide](docs/guide.md) for details")
    #expect(links == ["docs/guide.md"])
}

@Test func extractsMultipleLinks() {
    let links = extractLinks(from: "[a](one.md) text [b](two.md)")
    #expect(links == ["one.md", "two.md"])
}

@Test func stripsFragment() {
    let links = extractLinks(from: "[ref](page.md#section)")
    #expect(links == ["page.md"])
}

@Test func handlesRelativeDot() {
    let links = extractLinks(from: "[x](./local.md)")
    #expect(links == ["./local.md"])
}

@Test func handlesParentTraversal() {
    let links = extractLinks(from: "[x](../other/file.md)")
    #expect(links == ["../other/file.md"])
}

@Test func skipsHttpUrls() {
    let links = extractLinks(from: "[site](https://example.com/page.md)")
    #expect(links.isEmpty)
}

@Test func skipsHttpUrls2() {
    let links = extractLinks(from: "[site](http://example.com/page.md)")
    #expect(links.isEmpty)
}

@Test func skipsNonMarkdownLinks() {
    let links = extractLinks(from: "[img](photo.png) [script](app.ts)")
    #expect(links.isEmpty)
}

@Test func skipsHtmlTags() {
    let links = extractLinks(from: "<a href=\"page.md\">link</a>")
    #expect(links.isEmpty)
}

@Test func handlesEmptyInput() {
    let links = extractLinks(from: "")
    #expect(links.isEmpty)
}

@Test func handlesNoLinks() {
    let links = extractLinks(from: "Just some plain text without any links.")
    #expect(links.isEmpty)
}

@Test func handlesMultipleFragments() {
    // Only the first # counts
    let links = extractLinks(from: "[x](page.md#one#two)")
    #expect(links == ["page.md"])
}

@Test func skipsLinkSpanningLines() {
    let links = extractLinks(from: "[text](broken\nlink.md)")
    #expect(links.isEmpty)
}

@Test func handlesAdjacentLinks() {
    let links = extractLinks(from: "[a](a.md)[b](b.md)")
    #expect(links == ["a.md", "b.md"])
}

@Test func skipsShortPaths() {
    // ".md" alone is only 3 chars, need at least 4 (x.md)
    let links = extractLinks(from: "[x](.md)")
    #expect(links.isEmpty)
}

// MARK: - extractLinks (wiki links)

@Test func extractsWikiLink() {
    let links = extractLinks(from: "See [[guide.md]] for details")
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithAlias() {
    let links = extractLinks(from: "See [[guide.md|the guide]] for details")
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithFragment() {
    let links = extractLinks(from: "See [[guide.md#section]] here")
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithFragmentAndAlias() {
    let links = extractLinks(from: "See [[guide.md#section|display]] here")
    #expect(links == ["guide.md"])
}

@Test func extractsWikiLinkWithPath() {
    let links = extractLinks(from: "See [[docs/guide.md]] for details")
    #expect(links == ["docs/guide.md"])
}

@Test func skipsWikiLinkWithoutExtension() {
    let links = extractLinks(from: "See [[guide]] here")
    #expect(links.isEmpty)
}

@Test func skipsWikiLinkToNonMd() {
    let links = extractLinks(from: "See [[image.png]] here")
    #expect(links.isEmpty)
}

@Test func extractsMixedLinks() {
    let links = extractLinks(from: "[[wiki.md]] and [standard](standard.md)")
    #expect(links == ["wiki.md", "standard.md"])
}

@Test func skipsEmptyWikiLink() {
    let links = extractLinks(from: "See [[]] here")
    #expect(links.isEmpty)
}

@Test func skipsWikiLinkSpanningLines() {
    let links = extractLinks(from: "See [[broken\nlink]] here")
    #expect(links.isEmpty)
}

@Test func extractsAdjacentWikiLinks() {
    let links = extractLinks(from: "[[one.md]][[two.md]]")
    #expect(links == ["one.md", "two.md"])
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

@Test func bfsCrawlFindsBrokenLinks() {
    withTempDir { root in
        writeFile("\(root)/index.md", "[a](missing.md)")
        let allFiles = discoverFiles(root: root)
        let (_, broken) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root, allFiles: allFiles)
        #expect(broken.count == 1)
        #expect(broken[0].link == "missing.md")
    }
}

@Test func bfsCrawlNoBrokenOnValidLinks() {
    withTempDir { root in
        writeFile("\(root)/index.md", "[a](other.md)")
        writeFile("\(root)/other.md", "hello")
        let allFiles = discoverFiles(root: root)
        let (reachable, broken) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root, allFiles: allFiles)
        #expect(broken.isEmpty)
        #expect(reachable.count == 2)
    }
}

@Test func bfsCrawlBasenameFallback() {
    withTempDir { root in
        mkdir("\(root)/docs", 0o755)
        writeFile("\(root)/index.md", "[a](guide.md)")
        writeFile("\(root)/docs/guide.md", "hello")
        let allFiles = discoverFiles(root: root)
        let (reachable, broken) = bfsCrawl(entryPaths: ["\(root)/index.md"], root: root, allFiles: allFiles)
        #expect(broken.isEmpty)
        #expect(reachable.count == 2)
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
