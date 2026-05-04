import Darwin

/// One link extracted from markdown. Carries enough byte-offset info to feed the cache layer
/// and `--fix` byte rewriter without a separate "cached" or "detailed" variant.
public struct Link: Codable, Equatable {
    public enum Kind: Codable, Equatable {
        case wiki                       // [[target]]
        case standard                   // [text](target)
        case crossRepo(repo: String)    // `target` (repo)
    }

    public let kind: Kind
    public let target: String   // path before fragment / alias
    public let fragment: String?
    public let pathStart: Int   // byte offset of `target` start in source
    public let pathEnd: Int     // exclusive

    public init(kind: Kind, target: String, fragment: String?, pathStart: Int, pathEnd: Int) {
        self.kind = kind
        self.target = target
        self.fragment = fragment
        self.pathStart = pathStart
        self.pathEnd = pathEnd
    }
}

// MARK: - Link extraction

/// Scan raw UTF-8 bytes for markdown links to local files.
/// Manual byte scan — Swift Regex is 28-33x slower: https://forums.swift.org/t/slow-regex-performance/75768
public func extractLinks(_ buf: UnsafeBufferPointer<UInt8>) -> [Link] {
    guard let base = buf.baseAddress, buf.count > 4 else { return [] }
    let count = buf.count
    var links: [Link] = []
    var i = 0
    var inFence = false

    while i < count {
        // Code-fence detection at line start (\n or BOF followed by ```).
        // Skip indented fences — only flush-left ``` toggles fence state.
        let atLineStart = (i == 0 || base[i - 1] == 0x0A)
        if atLineStart && i + 2 < count
            && base[i] == 0x60 && base[i + 1] == 0x60 && base[i + 2] == 0x60 {
            inFence.toggle()
            // Skip to end of line so the fence's own info-string isn't scanned.
            while i < count && base[i] != 0x0A { i += 1 }
            if i < count { i += 1 }
            continue
        }

        if inFence {
            i += 1
            continue
        }

        if i < count - 1 {
            if base[i] == 0x5B, base[i + 1] == 0x5B {
                i = scanWikiLink(base, count: count, at: i, into: &links)
                continue
            }
            if base[i] == 0x5D, base[i + 1] == 0x28 {
                i = scanStandardLink(base, count: count, at: i, into: &links)
                continue
            }
        }
        if base[i] == 0x60 {
            i = scanBacktickRef(base, count: count, at: i, into: &links)
            continue
        }
        i += 1
    }

    return links
}

/// Convenience: extract links from a String.
public func extractLinks(from string: String) -> [Link] {
    var str = string
    return str.withUTF8 { extractLinks($0) }
}

// MARK: - Per-syntax scanners

/// Parse [[page]], [[page|alias]], [[page#section]] wiki links.
private func scanWikiLink(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [Link]
) -> Int {
    let start = i + 2
    var end = start
    while end < count - 1 {
        let b = base[end]
        if b == 0x0A || b == 0x0D { break }
        if b == 0x5D, base[end + 1] == 0x5D { break }
        end += 1
    }
    guard end < count - 1, base[end] == 0x5D, base[end + 1] == 0x5D, end > start else {
        return end + 1
    }

    var nameEnd = end
    var hashPos = -1
    for j in start..<end {
        if base[j] == 0x23 { hashPos = j; nameEnd = j; break }
        if base[j] == 0x7C { nameEnd = j; break }
    }
    var fragEnd = end
    if hashPos >= 0 {
        for j in (hashPos + 1)..<end {
            if base[j] == 0x7C { fragEnd = j; break }
        }
    }
    let nameLen = nameEnd - start
    guard nameLen > 0 else { return end + 2 }

    guard hasExtension(base, from: start, len: nameLen) else { return end + 2 }
    var fragment: String? = nil
    if hashPos >= 0 {
        let fragLen = fragEnd - hashPos - 1
        if fragLen > 0 { fragment = decodeUTF8(base, from: hashPos + 1, len: fragLen) }
    }
    links.append(Link(
        kind: .wiki,
        target: decodeUTF8(base, from: start, len: nameLen),
        fragment: fragment,
        pathStart: start,
        pathEnd: nameEnd
    ))
    return end + 2
}

/// Parse [text](path.md#fragment) standard links.
private func scanStandardLink(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [Link]
) -> Int {
    let start = i + 2
    var end = start
    var fragPos = -1
    while end < count {
        let b = base[end]
        if b == 0x29 || b == 0x0A || b == 0x0D { break }
        if b == 0x23 && fragPos < 0 { fragPos = end }
        end += 1
    }
    guard end < count, base[end] == 0x29 else { return end + 1 }

    let pathEnd = fragPos >= 0 ? fragPos : end
    let pathLen = pathEnd - start

    // Skip http(s) URLs
    if base[start] == 0x68, pathLen > 7,
       base[start + 1] == 0x74, base[start + 2] == 0x74, base[start + 3] == 0x70
    { return end + 1 }

    guard hasExtension(base, from: start, len: pathLen) else { return end + 1 }

    var fragment: String? = nil
    if fragPos >= 0 {
        let fragLen = end - fragPos - 1
        if fragLen > 0 { fragment = decodeUTF8(base, from: fragPos + 1, len: fragLen) }
    }
    links.append(Link(
        kind: .standard,
        target: decodeUTF8(base, from: start, len: pathLen),
        fragment: fragment,
        pathStart: start,
        pathEnd: pathEnd
    ))
    return end + 1
}

/// Parse `path` (repo-name) cross-repo backtick refs. Returns next scan index.
/// Recognizes `path.ext` (repo) and `path.ext#fragment` (repo).
/// Bare `path.ext` without trailing ` (repo)` is currently ignored (inline-code style is deferred).
private func scanBacktickRef(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [Link]
) -> Int {
    // Skip multi-backtick spans (``foo``, ```bar```) — only single-backtick code spans handled.
    if i + 1 < count && base[i + 1] == 0x60 { return i + 1 }

    let pathStart = i + 1
    var end = pathStart
    while end < count {
        let b = base[end]
        if b == 0x60 { break }
        if b == 0x0A || b == 0x0D { return i + 1 }  // unclosed backtick on this line → advance past it
        end += 1
    }
    guard end < count, base[end] == 0x60, end > pathStart else { return i + 1 }

    // After the closing backtick, look for " (repo-name)".
    let afterClose = end + 1
    guard afterClose + 2 < count, base[afterClose] == 0x20, base[afterClose + 1] == 0x28 else {
        // Bare `path.ext` — inline-code style (deferred); skip past the closing backtick.
        return afterClose
    }
    let repoStart = afterClose + 2
    var repoEnd = repoStart
    while repoEnd < count {
        let rb = base[repoEnd]
        if rb == 0x29 { break }
        // Repo names: [A-Za-z0-9_-]
        let isAlpha = (rb >= 0x41 && rb <= 0x5A) || (rb >= 0x61 && rb <= 0x7A)
        let isDigit = rb >= 0x30 && rb <= 0x39
        if !(isAlpha || isDigit || rb == 0x5F || rb == 0x2D) { return afterClose }
        repoEnd += 1
    }
    guard repoEnd < count, base[repoEnd] == 0x29, repoEnd > repoStart else { return afterClose }

    // Path inside backticks: split on first '#' for fragment.
    var hashPos = -1
    for j in pathStart..<end {
        if base[j] == 0x23 { hashPos = j; break }
    }
    let nameEnd = hashPos >= 0 ? hashPos : end
    let nameLen = nameEnd - pathStart
    guard nameLen > 0, hasExtension(base, from: pathStart, len: nameLen) else {
        return repoEnd + 1
    }

    var fragment: String? = nil
    if hashPos >= 0 {
        let fragLen = end - hashPos - 1
        if fragLen > 0 { fragment = decodeUTF8(base, from: hashPos + 1, len: fragLen) }
    }
    let repo = decodeUTF8(base, from: repoStart, len: repoEnd - repoStart)
    links.append(Link(
        kind: .crossRepo(repo: repo),
        target: decodeUTF8(base, from: pathStart, len: nameLen),
        fragment: fragment,
        pathStart: pathStart,
        pathEnd: nameEnd
    ))
    return repoEnd + 1
}

// MARK: - Heading extraction

/// Convert heading text to GitHub-style anchor ID.
/// Lowercase, keep letters/numbers/hyphens/underscores, spaces→hyphens.
public func anchorId(from text: String) -> String {
    var result = ""
    for ch in text.lowercased() {
        if ch.isLetter || ch.isNumber || ch == "-" || ch == "_" {
            result.append(ch)
        } else if ch == " " {
            result.append("-")
        }
    }
    return result
}

/// Extract heading anchors from markdown content (GitHub-style slugs).
public func extractHeadings(_ buf: UnsafeBufferPointer<UInt8>) -> Set<String> {
    guard let base = buf.baseAddress else { return [] }
    let count = buf.count
    var headings = Set<String>()
    var i = 0

    while i < count {
        if base[i] == 0x23 && (i == 0 || base[i - 1] == 0x0A || base[i - 1] == 0x0D) {
            var j = i
            while j < count && base[j] == 0x23 { j += 1 }
            if j < count && base[j] == 0x20 {
                j += 1
                let headStart = j
                while j < count && base[j] != 0x0A && base[j] != 0x0D { j += 1 }
                // Trim trailing whitespace
                var headEnd = j
                while headEnd > headStart && (base[headEnd - 1] == 0x20 || base[headEnd - 1] == 0x09) {
                    headEnd -= 1
                }
                if headEnd > headStart {
                    let text = decodeUTF8(base, from: headStart, len: headEnd - headStart)
                    headings.insert(anchorId(from: text))
                }
            }
            i = j + 1
        } else {
            while i < count && base[i] != 0x0A { i += 1 }
            i += 1
        }
    }

    return headings
}

/// Convenience: extract headings from a String.
public func extractHeadings(from string: String) -> Set<String> {
    var str = string
    return str.withUTF8 { extractHeadings($0) }
}
