import Darwin

// MARK: - Path utilities

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

/// Strip `root + "/"` prefix from `path` when present. Returns "" when `path == root`.
/// Returns nil when `path` is outside `root` (caller decides what to do).
public func relPath(_ path: String, under root: String) -> String? {
    if path == root { return "" }
    if path.hasPrefix(root + "/") { return String(path.dropFirst(root.count + 1)) }
    return nil
}

// MARK: - File reading

private var readBuffer = UnsafeMutablePointer<UInt8>.allocate(capacity: 256 * 1024)
private var readBufferCapacity = 256 * 1024

/// Read file contents into the reusable buffer. Returns (inode, buffer slice).
/// Buffer is only valid until the next call — callers must extract any needed data
/// before invoking `readFile` again.
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

// MARK: - Byte-scanner helpers (internal, shared by Extract.swift)

/// Check if the path segment has a file extension (a '.' followed by 1+ chars, not preceded by '/').
@inline(__always)
func hasExtension(_ base: UnsafePointer<UInt8>, from: Int, len: Int) -> Bool {
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
func decodeUTF8(_ base: UnsafePointer<UInt8>, from: Int, len: Int) -> String {
    String(decoding: UnsafeBufferPointer(start: base + from, count: len), as: UTF8.self)
}
