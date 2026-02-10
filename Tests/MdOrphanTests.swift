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

@Test func dirNameOfFilePath() {
    #expect(dirName("/repo/docs/file.md") == "/repo/docs")
}

@Test func dirNameOfRootFile() {
    #expect(dirName("/file.md") == "")
}

@Test func dirNameNoSlash() {
    #expect(dirName("file.md") == ".")
}
