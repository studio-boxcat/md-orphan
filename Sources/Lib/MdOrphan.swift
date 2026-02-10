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

/// Check if a relative path matches any exclude pattern.
/// Plain patterns match as path prefix (e.g. "Library" matches "Library/foo/bar.md").
/// Patterns with *, ?, [ are matched as globs via fnmatch(3).
public func isExcluded(_ relPath: String, by patterns: [String]) -> Bool {
    for pattern in patterns {
        if pattern.contains("*") || pattern.contains("?") || pattern.contains("[") {
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

@inline(__always)
private func endsMd(_ base: UnsafePointer<UInt8>, at pos: Int, len: Int) -> Bool {
    len >= 4 && base[pos - 1] == 0x64 && base[pos - 2] == 0x6D && base[pos - 3] == 0x2E
}

@inline(__always)
private func decodeUTF8(_ base: UnsafePointer<UInt8>, from: Int, len: Int) -> String {
    String(decoding: UnsafeBufferPointer(start: base + from, count: len), as: UTF8.self)
}

/// Scan raw UTF-8 bytes for markdown links to .md files.
/// Manual byte scan — Swift Regex is 28-33x slower: https://forums.swift.org/t/slow-regex-performance/75768
public func extractLinks(_ buf: UnsafeBufferPointer<UInt8>) -> [String] {
    guard let base = buf.baseAddress, buf.count > 4 else { return [] }
    let count = buf.count
    var links: [String] = []
    var i = 0

    while i < count - 1 {
        if base[i] == 0x5B, base[i + 1] == 0x5B {
            i = scanWikiLink(base, count: count, at: i, into: &links)
            continue
        }
        if base[i] == 0x5D, base[i + 1] == 0x28 {
            i = scanStandardLink(base, count: count, at: i, into: &links)
            continue
        }
        i += 1
    }

    return links
}

/// Parse [[page]], [[page|alias]], [[page#section]] wiki links.
private func scanWikiLink(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [String]
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
    for j in start..<end {
        if base[j] == 0x23 || base[j] == 0x7C { nameEnd = j; break }
    }
    let nameLen = nameEnd - start
    guard nameLen > 0 else { return end + 2 }

    if endsMd(base, at: nameEnd, len: nameLen) {
        links.append(decodeUTF8(base, from: start, len: nameLen))
    } else {
        var hasDot = false
        for j in start..<nameEnd { if base[j] == 0x2E { hasDot = true; break } }
        if !hasDot { links.append(decodeUTF8(base, from: start, len: nameLen) + ".md") }
    }
    return end + 2
}

/// Parse [text](path.md#fragment) standard links.
private func scanStandardLink(
    _ base: UnsafePointer<UInt8>, count: Int, at i: Int, into links: inout [String]
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
    guard endsMd(base, at: pathEnd, len: pathLen) else { return end + 1 }

    if base[start] == 0x68, pathLen > 7,
       base[start + 1] == 0x74, base[start + 2] == 0x74, base[start + 3] == 0x70
    { return end + 1 }

    links.append(decodeUTF8(base, from: start, len: pathLen))
    return end + 1
}

/// Convenience: extract links from a String.
public func extractLinks(from string: String) -> [String] {
    var str = string
    return str.withUTF8 { extractLinks($0) }
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
public func bfsCrawl(entryPaths: [String], root: String) -> Set<ino_t> {
    var reachable = Set<ino_t>()
    var queued = Set(entryPaths)
    var queue = entryPaths
    var idx = 0

    while idx < queue.count {
        let filePath = queue[idx]
        idx += 1

        guard let (inode, content) = readFile(path: filePath) else {
            fputs("md-orphan: warning: cannot read \(filePath)\n", stderr)
            continue
        }

        guard reachable.insert(inode).inserted else { continue }

        for link in extractLinks(content) {
            guard let resolved = resolveLink(link, relativeTo: filePath, root: root) else {
                continue
            }
            guard let canonical = realPath(resolved) else {
                fputs("md-orphan: warning: broken link \(link) in \(filePath)\n", stderr)
                continue
            }
            if queued.insert(canonical).inserted {
                queue.append(canonical)
            }
        }
    }

    return reachable
}
