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
///
/// Pattern semantics (gitignore-flavored):
/// - Trailing `/` makes it a directory pattern (`Library/`, `Pods/`).
///   - Bare basename (no `/` in middle, e.g. `Library/`) matches at ANY depth.
///   - Path-containing (`assets/loc/`) is anchored at root.
/// - Patterns with `*`, `?`, `[` are matched as globs via `fnmatch(3)` (PATHNAME mode — `*` doesn't cross `/`).
/// - Plain patterns without trailing `/` match as path prefix at root (`Library` matches `Library/foo`).
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
            } else if !prefix.contains("/") {
                // Bare basename — match anywhere in tree (gitignore semantics).
                // Equivalent to scanning `relPath`'s segments for an exact match.
                if relPath == prefix { return true }
                if relPath.hasPrefix(prefix + "/") { return true }
                if relPath.contains("/" + prefix + "/") { return true }
                if relPath.hasSuffix("/" + prefix) { return true }
            } else {
                // Path-anchored at root.
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

/// Pre-compiled exclude matcher. Built once per `indexRepo` call; consulted ~4k+ times during
/// the fts walk. Splits patterns into the three forms so the hot path (bare-basename match
/// during directory prune) becomes a single `Set<String>.contains` lookup against the dir's
/// basename — no `relPath` allocation, no per-pattern string ops.
public struct ExcludeMatcher {
    /// Trailing-slash bare-basename patterns (e.g. `Library/`, `Pods/`). Match anywhere in tree.
    /// Stored without trailing slash.
    let bareBasenames: Set<String>
    /// Trailing-slash path-containing patterns (e.g. `docs/internal/`). Anchored at root.
    let anchored: [String]
    /// Patterns containing `*`, `?`, `[…]`. Matched via `fnmatch`.
    let globs: [String]
    /// Plain (no trailing slash, no glob) patterns — match as path prefix at root.
    let plainPrefixes: [String]
    /// Trailing-slash glob patterns (e.g. `assets/loc/*/`).
    let trailingGlobs: [(prefix: String, depth: Int)]

    public init(_ patterns: [String]) {
        var bare: Set<String> = []
        var anchored: [String] = []
        var globs: [String] = []
        var plain: [String] = []
        var trailingGlobs: [(String, Int)] = []
        for p in patterns {
            let isGlob = p.contains("*") || p.contains("?") || p.contains("[")
            if p.hasSuffix("/") {
                let prefix = String(p.dropLast())
                if isGlob {
                    let depth = prefix.split(separator: "/", omittingEmptySubsequences: true).count
                    trailingGlobs.append((prefix, depth))
                } else if !prefix.contains("/") {
                    bare.insert(prefix)
                } else {
                    anchored.append(p)
                }
            } else if isGlob {
                globs.append(p)
            } else {
                plain.append(p)
            }
        }
        self.bareBasenames = bare
        self.anchored = anchored
        self.globs = globs
        self.plainPrefixes = plain
        self.trailingGlobs = trailingGlobs
    }

    /// Fast basename-only check — matches a bare-basename pattern at any depth without
    /// constructing the full relPath. Returns true if the bare set contains `basename`.
    @inline(__always)
    public func matchesBare(basename: String) -> Bool {
        bareBasenames.contains(basename)
    }

    /// Full relPath check — used after `matchesBare` returns false, or when the caller
    /// already has the relPath in hand (file-level checks).
    public func matches(relPath: String, basename: String? = nil) -> Bool {
        // Bare set: any path segment in relPath could match.
        // The caller usually has `basename` already (fts gives it cheap); use it when present.
        if let basename, bareBasenames.contains(basename) { return true }
        if !bareBasenames.isEmpty {
            // Fall back to scanning segments — needed when caller can't isolate basename
            // (e.g. file-path check where the parent dir's basename matters too).
            let segments = relPath.split(separator: "/", omittingEmptySubsequences: true)
            for seg in segments {
                if bareBasenames.contains(String(seg)) { return true }
            }
        }
        for prefix in plainPrefixes {
            if relPath == prefix || relPath.hasPrefix(prefix + "/") { return true }
        }
        for pattern in anchored {
            if relPath.hasPrefix(pattern) { return true }
        }
        for pattern in globs {
            if fnmatch(pattern, relPath, FNM_PATHNAME) == 0 { return true }
        }
        for (prefix, depth) in trailingGlobs {
            let parts = relPath.split(separator: "/", omittingEmptySubsequences: true)
            if parts.count > depth {
                let dir = parts[..<depth].joined(separator: "/")
                if fnmatch(prefix, dir, FNM_PATHNAME) == 0 { return true }
            }
        }
        return false
    }
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

/// Read file contents into the reusable buffer. Returns the buffer slice.
/// Buffer is only valid until the next call — callers must extract any needed data
/// before invoking `readFile` again.
/// read() beats mmap for small files — https://medium.com/cosmos-code/mmap-vs-read-a-performance-comparison-for-efficient-file-access-3e5337bd1e25
public func readFile(path: String) -> UnsafeBufferPointer<UInt8>? {
    path.withCString { cstr in
        let fd = open(cstr, O_RDONLY)
        guard fd >= 0 else { return nil }
        defer { close(fd) }

        var s = stat()
        guard fstat(fd, &s) == 0 else { return nil }

        let size = Int(s.st_size)
        if size == 0 { return UnsafeBufferPointer(start: nil, count: 0) }

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

        return UnsafeBufferPointer(start: UnsafePointer(readBuffer), count: totalRead)
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
