import Darwin

private var readBuffer = UnsafeMutablePointer<UInt8>.allocate(capacity: 256 * 1024)
private var readBufferCapacity = 256 * 1024

public func realPath(_ path: String) -> String? {
    var buf = [CChar](repeating: 0, count: Int(PATH_MAX))
    guard realpath(path, &buf) != nil else { return nil }
    return String(cString: buf)
}

public func dirName(_ path: String) -> String {
    if let idx = path.lastIndex(of: "/") {
        return String(path[..<idx])
    }
    return "."
}

public func baseName(_ path: String) -> String {
    if let idx = path.lastIndex(of: "/") {
        return String(path[path.index(after: idx)...])
    }
    return path
}

/// Check if a relative path matches any exclude pattern.
/// Plain patterns match as path prefix (e.g. "Library" matches "Library/foo/bar.md").
/// Trailing "/" treats pattern as directory prefix (e.g. "assets/loc/*/" matches everything under matching dirs).
/// Patterns with *, ?, [ are matched as globs via fnmatch(3).
public func isExcluded(_ relPath: String, by patterns: [String]) -> Bool {
    func isGlob(_ s: String) -> Bool { s.contains("*") || s.contains("?") || s.contains("[") }
    for pattern in patterns {
        if pattern.hasSuffix("/") {
            let prefix = String(pattern.dropLast())
            if isGlob(prefix) {
                let parts = relPath.split(separator: "/", omittingEmptySubsequences: true)
                let depth = prefix.split(separator: "/", omittingEmptySubsequences: true).count
                if parts.count > depth {
                    let dir = parts[..<depth].joined(separator: "/")
                    if fnmatch(prefix, dir, FNM_PATHNAME) == 0 { return true }
                }
            } else {
                if relPath.hasPrefix(pattern) { return true }
            }
        } else if isGlob(pattern) {
            if fnmatch(pattern, relPath, FNM_PATHNAME) == 0 { return true }
        } else {
            if relPath == pattern || relPath.hasPrefix(pattern + "/") { return true }
        }
    }
    return false
}

/// Walk `root` with fts, return [inode: relativePath] for all .md files.
/// fts_open with FTS_NOSTAT is fastest on APFS — http://blog.tempel.org/2019/04/dir-read-performance.html
/// Avoid getattrlistbulk — https://developer.apple.com/forums/thread/656787
public func discoverFiles(root: String, exclude: [String] = []) -> [ino_t: String] {
    let rootCStr = strdup(root)!
    defer { free(rootCStr) }
    var argv: [UnsafeMutablePointer<CChar>?] = [rootCStr, nil]

    guard let stream = fts_open(&argv, FTS_PHYSICAL | FTS_NOCHDIR | FTS_NOSTAT, nil) else {
        return [:]
    }
    defer { fts_close(stream) }

    let rootLen = root.utf8.count
    var allFiles: [ino_t: String] = [:]

    while let entry = fts_read(stream) {
        let info = Int32(entry.pointee.fts_info)
        let nameLen = Int(entry.pointee.fts_namelen)
        let namePtr = entry.pointee.fts_path
            .advanced(by: Int(entry.pointee.fts_pathlen) - nameLen)

        if info == FTS_D {
            if nameLen > 1 && namePtr.pointee == 0x2E {
                fts_set(stream, entry, FTS_SKIP)
                continue
            }
            if !exclude.isEmpty {
                let absPath = String(cString: entry.pointee.fts_path)
                let relPath = absPath.utf8.count > rootLen + 1
                    ? String(absPath.dropFirst(rootLen + 1))
                    : ""
                if !relPath.isEmpty && isExcluded(relPath, by: exclude) {
                    fts_set(stream, entry, FTS_SKIP)
                }
            }
            continue
        }

        guard info == FTS_F || info == FTS_SL || info == FTS_NSOK else { continue }

        guard nameLen >= 3,
              namePtr[nameLen - 3] == 0x2E,
              namePtr[nameLen - 2] == 0x6D,
              namePtr[nameLen - 1] == 0x64
        else { continue }

        let absPath = String(cString: entry.pointee.fts_path)
        let relPath = absPath.utf8.count > rootLen + 1
            ? String(absPath.dropFirst(rootLen + 1))
            : absPath

        if !exclude.isEmpty && isExcluded(relPath, by: exclude) { continue }

        var s = stat()
        guard stat(entry.pointee.fts_path, &s) == 0 else { continue }
        allFiles[s.st_ino] = relPath
    }

    return allFiles
}

/// Read file contents into the reusable buffer. Returns (inode, buffer slice).
/// Buffer is only valid until the next call.
/// read() beats mmap for small files — https://medium.com/cosmos-code/mmap-vs-read-a-performance-comparison-for-efficient-file-access-3e5337bd1e25
public func readFile(path: String) -> (ino_t, UnsafeBufferPointer<UInt8>)? {
    path.withCString { cstr in
        let fd = open(cstr, O_RDONLY)
        guard fd >= 0 else { return nil }
        defer { close(fd) }

        var s = stat()
        guard fstat(fd, &s) == 0 else { return nil }

        let size = Int(s.st_size)
        let inode = s.st_ino

        if size == 0 {
            return (inode, UnsafeBufferPointer(start: nil, count: 0))
        }

        if size > readBufferCapacity {
            readBuffer.deallocate()
            readBufferCapacity = size
            readBuffer = .allocate(capacity: size)
        }

        var totalRead = 0
        while totalRead < size {
            let n = read(fd, readBuffer + totalRead, size - totalRead)
            if n <= 0 { break }
            totalRead += n
        }

        return (inode, UnsafeBufferPointer(start: UnsafePointer(readBuffer), count: totalRead))
    }
}

/// Check if the path segment has a file extension (a '.' followed by 1+ chars, not preceded by '/').
@inline(__always)
private func hasExtension(_ base: UnsafePointer<UInt8>, from: Int, len: Int) -> Bool {
    guard len >= 3 else { return false } // minimum: "a.b"
    var i = from + len - 1
    while i > from {
        let b = base[i]
        if b == 0x2E { return true }  // '.' found with chars after it
        if b == 0x2F { return false } // hit '/' before finding '.'
        i -= 1
    }
    return false
}

@inline(__always)
private func decodeUTF8(_ base: UnsafePointer<UInt8>, from: Int, len: Int) -> String {
    String(decoding: UnsafeBufferPointer(start: base + from, count: len), as: UTF8.self)
}

public struct MdLink: Equatable {
    public let path: String
    public let fragment: String?
}

public enum LinkKind: Equatable {
    case wiki                  // [[path]]
    case standard              // [text](path)
    case crossRepo(String)     // `path` (repo-name)
}

/// Link with byte positions for in-place rewriting (used by the cache layer too).
public struct MdLinkDetail: Equatable {
    public let path: String
    public let fragment: String?
    public let kind: LinkKind
    public let pathStart: Int  // byte offset of path start in source
    public let pathEnd: Int    // byte offset of path end (exclusive)

    public init(path: String, fragment: String?, kind: LinkKind, pathStart: Int, pathEnd: Int) {
        self.path = path
        self.fragment = fragment
        self.kind = kind
        self.pathStart = pathStart
        self.pathEnd = pathEnd
    }
}

/// Scan raw UTF-8 bytes for markdown links to local files.
/// Manual byte scan — Swift Regex is 28-33x slower: https://forums.swift.org/t/slow-regex-performance/75768
public func extractLinksWithFragments(_ buf: UnsafeBufferPointer<UInt8>) -> [MdLink] {
    extractLinksDetailed(buf).map { MdLink(path: $0.path, fragment: $0.fragment) }
}

public func extractLinks(_ buf: UnsafeBufferPointer<UInt8>) -> [String] {
    extractLinksDetailed(buf).map(\.path)
}

public func extractLinksDetailed(_ buf: UnsafeBufferPointer<UInt8>) -> [MdLinkDetail] {
    guard let base = buf.baseAddress, buf.count > 4 else { return [] }
    let count = buf.count
    var links: [MdLinkDetail] = []
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

/// Parse [[page]], [[page|alias]], [[page#section]] wiki links.
private func scanWikiLink(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [MdLinkDetail]
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
    links.append(MdLinkDetail(
        path: decodeUTF8(base, from: start, len: nameLen),
        fragment: fragment,
        kind: .wiki,
        pathStart: start,
        pathEnd: nameEnd
    ))
    return end + 2
}

/// Parse [text](path.md#fragment) standard links.
private func scanStandardLink(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [MdLinkDetail]
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
    links.append(MdLinkDetail(
        path: decodeUTF8(base, from: start, len: pathLen),
        fragment: fragment,
        kind: .standard,
        pathStart: start,
        pathEnd: pathEnd
    ))
    return end + 1
}

/// Parse `path` (repo-name) cross-repo backtick refs. Returns next scan index.
/// Recognizes `path.ext` (repo) and `path.ext#fragment` (repo).
/// Bare `path.ext` without trailing ` (repo)` is currently ignored (inline-code style is deferred).
private func scanBacktickRef(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [MdLinkDetail]
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
    links.append(MdLinkDetail(
        path: decodeUTF8(base, from: pathStart, len: nameLen),
        fragment: fragment,
        kind: .crossRepo(repo),
        pathStart: pathStart,
        pathEnd: nameEnd
    ))
    return repoEnd + 1
}

/// Convenience: extract links from a String.
public func extractLinks(from string: String) -> [String] {
    var str = string
    return str.withUTF8 { extractLinks($0) }
}

/// Convenience: extract links with fragments from a String.
public func extractLinksWithFragments(from string: String) -> [MdLink] {
    var str = string
    return str.withUTF8 { extractLinksWithFragments($0) }
}

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

/// Resolve a link relative to the file containing it. Returns absolute path or nil if escapes root.
public func resolveLink(_ link: String, relativeTo sourceFile: String, root: String) -> String? {
    let sourceDir = dirName(sourceFile)

    let combined = link.hasPrefix("/")
        ? root + link
        : sourceDir + "/" + link

    var segments: [String] = []
    for seg in combined.split(separator: "/", omittingEmptySubsequences: true) {
        switch seg {
        case ".": continue
        case "..": if !segments.isEmpty { segments.removeLast() }
        default: segments.append(String(seg))
        }
    }

    let resolved = "/" + segments.joined(separator: "/")

    guard resolved == root || resolved.hasPrefix(root + "/") else {
        return nil
    }

    return resolved
}

/// BFS from entry points. Returns set of reachable inodes.
/// Single-threaded — TaskGroup overhead exceeds I/O at this scale: https://forums.swift.org/t/taskgroup-and-parallelism/51039
/// When a link can't be resolved relative to its file, falls back to basename lookup in allFiles.
public struct LinkIssue: Equatable {
    public enum StyleScope: Equatable {
        case wiki                       // [[path]]
        case crossRepo(repo: String)    // `path` (repo)
    }
    public enum Kind: Equatable {
        case broken
        case ambiguous(Int)
        case brokenAnchor(String)
        case style(scope: StyleScope, suggested: String, pathStart: Int, pathEnd: Int)
        case unknownRepo(repo: String)
        case crossRepoBroken(repo: String)
    }
    public let link: String
    public let source: String
    public let kind: Kind
}

public struct CrawlOptions {
    /// repo-name → absolute root (already env/tilde-expanded; will be realpath'd internally).
    public var repos: [String: String]
    public var useDefaultExcludes: Bool
    /// CLI --exclude entries; layered on top of <repo>/.md-orphan and built-in defaults.
    public var extraExcludes: [String]

    public init(repos: [String: String] = [:], useDefaultExcludes: Bool = true, extraExcludes: [String] = []) {
        self.repos = repos
        self.useDefaultExcludes = useDefaultExcludes
        self.extraExcludes = extraExcludes
    }
}

/// BFS from entry points across same-repo and (optionally) cross-repo references.
///
/// Visited tracking is split:
///  - **Entry repo**: inode-keyed (`reachable`). Used for orphan detection + symlink/hardlink dedup.
///  - **Cross-repo**: canonical-path-keyed. Cross-repo files are visited + verified + style-checked
///    but never enter `reachable` (orphan detection is scoped to the entry repo).
///
/// Single-threaded — TaskGroup overhead exceeds I/O at this scale: https://forums.swift.org/t/taskgroup-and-parallelism/51039
public func bfsCrawl(
    entryPaths: [String],
    root: String,
    allFiles: [ino_t: String] = [:],
    options: CrawlOptions = .init(),
    cache: ExtractionCache = ExtractionCache(enabled: false)
) -> (reachable: Set<ino_t>, issues: [LinkIssue]) {
    var state = CrawlState(root: root, allFiles: allFiles, options: options, cache: cache)
    state.seed(entryPaths)
    while let item = state.dequeue() {
        state.visit(item)
    }
    state.pruneCache()
    return (state.reachable, state.issues)
}

struct BfsQueueItem { let path: String; let repoRoot: String; let isEntryRepo: Bool }

/// All `bfsCrawl` mutable state lives here. Methods walk the graph, resolve links, and emit issues.
/// Constructed and dropped within a single `bfsCrawl` call — no shared instance, no statics.
struct CrawlState {
    // Immutable context
    let entryRoot: String                  // canonical (realpath'd)
    let entryIndex: RepoIndex
    let resolvedRepos: [String: String]    // repo-name → canonical root
    let options: CrawlOptions
    let cache: ExtractionCache

    // Mutable BFS state
    var indices: [String: RepoIndex]                       // canonical root → index (lazy fill)
    var queue: [BfsQueueItem] = []
    var cursor: Int = 0
    var queuedEntryPaths: Set<String> = []                  // dedup before read, entry repo
    var crossRepoVisited: Set<String> = []                  // canonical paths for cross-repo files
    var reachable: Set<ino_t> = []                          // entry-repo orphan tracking
    var issues: [LinkIssue] = []
    var headingCache: [String: Set<String>] = [:]

    init(root: String, allFiles: [ino_t: String], options: CrawlOptions, cache: ExtractionCache) {
        // Resolve root to canonical (realpath). Mixing resolved/unresolved paths breaks prefix
        // checks (e.g. macOS /var/folders → /private/var/folders symlink).
        let resolved = realPath(root) ?? root
        self.entryRoot = resolved
        self.options = options
        self.cache = cache

        // Entry-repo index: synthesize from `allFiles` (legacy/test path) or build fresh.
        if !allFiles.isEmpty {
            var byName: [String: [String]] = [:]
            for (_, relPath) in allFiles {
                byName[baseName(relPath), default: []].append(resolved + "/" + relPath)
            }
            self.entryIndex = RepoIndex(root: resolved, mdFiles: allFiles, byName: byName, exclude: [])
        } else {
            let projectIgnore = (try? loadProjectIgnore(root: resolved)) ?? []
            let exclude = options.extraExcludes + projectIgnore
            self.entryIndex = indexRepo(root: resolved, exclude: exclude,
                                        useDefaultExcludes: options.useDefaultExcludes)
        }

        var resolvedRepos: [String: String] = [:]
        for (name, raw) in options.repos {
            resolvedRepos[name] = realPath(raw) ?? raw
        }
        self.resolvedRepos = resolvedRepos
        self.indices = [resolved: entryIndex]
    }

    // MARK: - Queue control

    mutating func seed(_ entryPaths: [String]) {
        let canonicals = entryPaths.map { realPath($0) ?? $0 }
        for c in canonicals {
            queuedEntryPaths.insert(c)
            queue.append(BfsQueueItem(path: c, repoRoot: entryRoot, isEntryRepo: true))
        }
    }

    mutating func dequeue() -> BfsQueueItem? {
        guard cursor < queue.count else { return nil }
        defer { cursor += 1 }
        return queue[cursor]
    }

    mutating func visit(_ item: BfsQueueItem) {
        guard let result = cache.read(filePath: item.path, repoRoot: item.repoRoot) else {
            fputs("md-orphan: warning: cannot read \(item.path)\n", stderr)
            return
        }
        if item.isEntryRepo {
            guard reachable.insert(result.inode).inserted else { return }
        } else {
            guard crossRepoVisited.insert(item.path).inserted else { return }
        }
        // Cache source headings — cheap because we already have the bytes.
        headingCache[item.path] = result.headings

        for link in result.links { resolve(link, source: item) }
    }

    // MARK: - Lookups

    mutating func indexFor(_ canonicalRoot: String) -> RepoIndex {
        if let cached = indices[canonicalRoot] { return cached }
        let projectIgnore = (try? loadProjectIgnore(root: canonicalRoot)) ?? []
        let exclude = options.extraExcludes + projectIgnore
        let idx = indexRepo(root: canonicalRoot, exclude: exclude,
                            useDefaultExcludes: options.useDefaultExcludes)
        indices[canonicalRoot] = idx
        return idx
    }

    mutating func headings(for canonical: String) -> Set<String> {
        if let cached = headingCache[canonical] { return cached }
        let owningRoot = repoRootContaining(canonical) ?? entryRoot
        let h: Set<String>
        if let result = cache.read(filePath: canonical, repoRoot: owningRoot) {
            h = result.headings
        } else {
            h = []
        }
        headingCache[canonical] = h
        return h
    }

    /// Which configured repo (if any) a canonical absolute path lives under.
    func repoRootContaining(_ path: String) -> String? {
        if path == entryRoot || path.hasPrefix(entryRoot + "/") { return entryRoot }
        for (_, root) in resolvedRepos {
            if path == root || path.hasPrefix(root + "/") { return root }
        }
        return nil
    }

    // MARK: - Link resolution

    mutating func resolve(_ link: MdLinkDetail, source: BfsQueueItem) {
        switch link.kind {
        case .wiki, .standard:
            resolveSameRepo(link, source: source)
        case .crossRepo(let repoName):
            resolveCrossRepo(link, source: source, repoName: repoName)
        }
    }

    private mutating func resolveSameRepo(_ link: MdLinkDetail, source: BfsQueueItem) {
        let current = indices[source.repoRoot] ?? entryIndex
        guard let resolved = resolveLink(link.path, relativeTo: source.path, root: current.root) else {
            return
        }
        let isMd = link.path.hasSuffix(".md")
        var canonical = realPath(resolved)

        // Basename fallback only for .md links.
        if canonical == nil && isMd {
            let basename = baseName(link.path)
            if let candidates = current.byName[basename], candidates.count == 1 {
                canonical = realPath(candidates[0])
            } else if let candidates = current.byName[basename], candidates.count > 1 {
                issues.append(LinkIssue(link: link.path, source: source.path,
                                        kind: .ambiguous(candidates.count)))
                return
            }
        }
        guard let canonical else {
            issues.append(LinkIssue(link: link.path, source: source.path, kind: .broken))
            return
        }

        // Style check: wiki links only, target must live in same repo.
        if link.kind == .wiki, canonical.hasPrefix(current.root + "/") {
            emitStyleIfNeeded(link: link, source: source.path,
                              canonical: canonical, repoRoot: current.root,
                              byName: current.byName, scope: .wiki)
        }

        guard isMd else { return }

        if let fragment = link.fragment, !headings(for: canonical).contains(fragment) {
            issues.append(LinkIssue(link: link.path, source: source.path,
                                    kind: .brokenAnchor(fragment)))
        }

        // Enqueue: usually same-repo entry-repo dedup; cross-repo if symlink crosses boundary.
        let owningRoot = repoRootContaining(canonical) ?? current.root
        enqueueResolved(canonical: canonical, owningRoot: owningRoot)
    }

    private mutating func resolveCrossRepo(_ link: MdLinkDetail, source: BfsQueueItem, repoName: String) {
        guard let repoRoot = resolvedRepos[repoName] else {
            issues.append(LinkIssue(link: link.path, source: source.path,
                                    kind: .unknownRepo(repo: repoName)))
            return
        }

        // Cross-repo paths are repo-root-relative; normalize ./ and ../ but reject root escape.
        let rawCombined = link.path.hasPrefix("/") ? repoRoot + link.path : repoRoot + "/" + link.path
        var segments: [String] = []
        var escaped = false
        for seg in rawCombined.split(separator: "/", omittingEmptySubsequences: true) {
            switch seg {
            case ".": continue
            case "..":
                if !segments.isEmpty { segments.removeLast() } else { escaped = true; break }
            default: segments.append(String(seg))
            }
        }
        let normalized = "/" + segments.joined(separator: "/")
        let withinRoot = !escaped && (normalized == repoRoot || normalized.hasPrefix(repoRoot + "/"))

        let isMd = link.path.hasSuffix(".md")
        var canonical: String? = withinRoot ? realPath(normalized) : nil
        let repoIndex = indexFor(repoRoot)

        // Basename fallback in target repo — also handles `../` escapes for style violations.
        if canonical == nil && isMd {
            let basename = baseName(link.path)
            if let candidates = repoIndex.byName[basename], candidates.count == 1 {
                canonical = realPath(candidates[0])
            } else if let candidates = repoIndex.byName[basename], candidates.count > 1 {
                issues.append(LinkIssue(link: link.path, source: source.path,
                                        kind: .ambiguous(candidates.count)))
                return
            }
        }
        guard let canonical else {
            issues.append(LinkIssue(link: link.path, source: source.path,
                                    kind: .crossRepoBroken(repo: repoName)))
            return
        }

        if canonical.hasPrefix(repoRoot + "/") {
            emitStyleIfNeeded(link: link, source: source.path,
                              canonical: canonical, repoRoot: repoRoot,
                              byName: repoIndex.byName, scope: .crossRepo(repo: repoName))
        }

        guard isMd else { return }

        if let fragment = link.fragment, !headings(for: canonical).contains(fragment) {
            issues.append(LinkIssue(link: link.path, source: source.path,
                                    kind: .brokenAnchor(fragment)))
        }

        // Self-ref into entry repo (rare) funnels into entry-repo dedup.
        let owningRoot = (canonical == entryRoot || canonical.hasPrefix(entryRoot + "/"))
            ? entryRoot
            : repoRoot
        enqueueResolved(canonical: canonical, owningRoot: owningRoot)
    }

    private mutating func emitStyleIfNeeded(
        link: MdLinkDetail, source: String, canonical: String,
        repoRoot: String, byName: [String: [String]], scope: LinkIssue.StyleScope
    ) {
        let relTarget = String(canonical.dropFirst(repoRoot.count + 1))
        let basename = baseName(relTarget)
        let canonicalForm: String
        if let cands = byName[basename], cands.count == 1 {
            canonicalForm = basename
        } else {
            canonicalForm = relTarget
        }
        guard link.path != canonicalForm else { return }
        issues.append(LinkIssue(
            link: link.path, source: source,
            kind: .style(scope: scope, suggested: canonicalForm,
                         pathStart: link.pathStart, pathEnd: link.pathEnd)
        ))
    }

    private mutating func enqueueResolved(canonical: String, owningRoot: String) {
        if owningRoot == entryRoot {
            if queuedEntryPaths.insert(canonical).inserted {
                queue.append(BfsQueueItem(path: canonical, repoRoot: entryRoot, isEntryRepo: true))
            }
        } else if !crossRepoVisited.contains(canonical) {
            queue.append(BfsQueueItem(path: canonical, repoRoot: owningRoot, isEntryRepo: false))
        }
    }

    // MARK: - Cleanup

    mutating func pruneCache() {
        for (root, idx) in indices {
            let keep = Set(idx.mdFiles.values)
            cache.prune(canonicalRoot: root, keepRelativePaths: keep)
        }
    }
}
