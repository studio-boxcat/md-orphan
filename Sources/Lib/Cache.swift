import Darwin
import Foundation

/// Bumped on any change to the on-disk JSON shape. Files written under a different
/// schemaVersion are treated as a cache miss and silently overwritten.
let cacheSchemaVersion = 2

/// One cache file per repo lives at $XDG_CONFIG_HOME/md-orphan/cache/<hash>.json.
/// Filename is fnv1a64 of the canonical root path, so two repos with identical basenames
/// in different parents don't collide.
public struct LinkCache: Codable {
    public var schemaVersion: Int
    public var displayName: String   // human-readable, for debug — file is keyed by hash
    public var entries: [String: CacheEntry]   // key: relative path within the repo

    public init(displayName: String) {
        self.schemaVersion = cacheSchemaVersion
        self.displayName = displayName
        self.entries = [:]
    }
}

public struct CacheEntry: Codable {
    public let mtimeNs: Int64
    public let size: Int64
    public let contentHash: UInt64
    public let links: [Link]
    public let headings: [String]
}

// MARK: - Cache directory + filename

public func cacheDirectory() -> String {
    let xdg = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"]
    let base = (xdg?.isEmpty == false ? xdg! : (homeDir() + "/.config"))
    return base + "/md-orphan/cache"
}

public func cacheFilePath(forCanonicalRoot canonicalRoot: String) -> String {
    return cacheDirectory() + "/" + fnv1a64Hex(canonicalRoot) + ".json"
}

/// fnv1a-64 of a UTF-8 string. Used for cache filenames and for content-equivalence checks.
@inline(__always)
public func fnv1a64Hex(_ s: String) -> String {
    var h: UInt64 = 0xcbf29ce484222325
    for byte in s.utf8 {
        h ^= UInt64(byte)
        h = h &* 0x100000001b3
    }
    return String(h, radix: 16)
}

@inline(__always)
public func fnv1a64(_ buf: UnsafeBufferPointer<UInt8>) -> UInt64 {
    var h: UInt64 = 0xcbf29ce484222325
    guard let base = buf.baseAddress else { return h }
    for i in 0..<buf.count {
        h ^= UInt64(base[i])
        h = h &* 0x100000001b3
    }
    return h
}

// MARK: - Load / save

/// Load cache for a canonical repo root. Returns a fresh empty cache on any error or version mismatch.
public func loadLinkCache(canonicalRoot: String, displayName: String) -> LinkCache {
    let path = cacheFilePath(forCanonicalRoot: canonicalRoot)
    guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)) else {
        return LinkCache(displayName: displayName)
    }
    guard let decoded = try? JSONDecoder().decode(LinkCache.self, from: data) else {
        return LinkCache(displayName: displayName)
    }
    if decoded.schemaVersion != cacheSchemaVersion {
        return LinkCache(displayName: displayName)
    }
    return decoded
}

/// Save cache atomically. Cache loss isn't fatal — silently swallow write errors.
public func saveLinkCache(_ cache: LinkCache, canonicalRoot: String) {
    let dir = cacheDirectory()
    _ = mkdir(dir, 0o755)  // best-effort; ignores EEXIST
    // Also create parent .config/md-orphan if missing
    var components = dir.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
    components.removeLast()
    let parent = "/" + components.joined(separator: "/")
    _ = mkdir(parent, 0o755)

    let path = cacheFilePath(forCanonicalRoot: canonicalRoot)
    do {
        let data = try JSONEncoder().encode(cache)
        try data.write(to: URL(fileURLWithPath: path), options: .atomic)
    } catch {
        // Cache is regenerable; do not surface errors.
    }
}

// MARK: - Cache-aware extraction

/// Extracted data for a file: links + headings + the source bytes' inode.
public struct ExtractedFile {
    public let inode: ino_t
    public let links: [Link]
    public let headings: Set<String>
}

/// Cache-aware reader. Stat → read → hash → cache lookup. On hit: skip extraction.
/// On miss (or hash mismatch): extract fresh and update the cache. Caller is responsible
/// for calling `save()` at the end of a run to persist dirty caches.
public final class ExtractionCache {
    private var caches: [String: LinkCache] = [:]      // canonical root → cache
    private var dirty: Set<String> = []                 // canonical roots needing save
    private var displayNames: [String: String] = [:]   // canonical root → display name
    public var enabled: Bool

    public init(enabled: Bool = true) {
        self.enabled = enabled
    }

    /// Read + extract a file. `repoRoot` must be canonical (realpath). The file path may be
    /// outside the repo root (e.g. an entry file passed by absolute path) — in that case
    /// caching is skipped and we always extract fresh.
    public func read(filePath: String, repoRoot: String) -> ExtractedFile? {
        guard let (inode, buf) = readFile(path: filePath) else { return nil }

        guard enabled,
              filePath == repoRoot || filePath.hasPrefix(repoRoot + "/")
        else {
            return ExtractedFile(
                inode: inode,
                links: extractLinks(buf),
                headings: extractHeadings(buf)
            )
        }

        let relKey = filePath == repoRoot ? "" : String(filePath.dropFirst(repoRoot.count + 1))

        // Stat for mtime + size. Buffer read above already gave us inode + size.
        var st = stat()
        guard stat(filePath, &st) == 0 else {
            return ExtractedFile(
                inode: inode,
                links: extractLinks(buf),
                headings: extractHeadings(buf)
            )
        }
        let mtimeNs = Int64(st.st_mtimespec.tv_sec) &* 1_000_000_000 &+ Int64(st.st_mtimespec.tv_nsec)
        let size = Int64(st.st_size)
        let hash = fnv1a64(buf)

        let cache = ensureLoaded(canonicalRoot: repoRoot)
        if let entry = cache.entries[relKey],
           entry.mtimeNs == mtimeNs, entry.size == size, entry.contentHash == hash {
            return ExtractedFile(inode: inode, links: entry.links, headings: Set(entry.headings))
        }

        // Cache miss: extract + update.
        let links = extractLinks(buf)
        let headings = extractHeadings(buf)
        let newEntry = CacheEntry(
            mtimeNs: mtimeNs, size: size, contentHash: hash,
            links: links,
            headings: Array(headings).sorted()
        )
        caches[repoRoot]!.entries[relKey] = newEntry
        dirty.insert(repoRoot)
        return ExtractedFile(inode: inode, links: links, headings: headings)
    }

    /// Drop entries for files no longer under the repo (auto-prune).
    public func prune(canonicalRoot: String, keepRelativePaths: Set<String>) {
        guard var cache = caches[canonicalRoot] else { return }
        let removed = cache.entries.keys.filter { !keepRelativePaths.contains($0) }
        if removed.isEmpty { return }
        for k in removed { cache.entries.removeValue(forKey: k) }
        caches[canonicalRoot] = cache
        dirty.insert(canonicalRoot)
    }

    public func registerDisplayName(_ name: String, for canonicalRoot: String) {
        displayNames[canonicalRoot] = name
    }

    public func save() {
        guard enabled else { return }
        for root in dirty {
            guard let cache = caches[root] else { continue }
            saveLinkCache(cache, canonicalRoot: root)
        }
        dirty.removeAll()
    }

    private func ensureLoaded(canonicalRoot: String) -> LinkCache {
        if let existing = caches[canonicalRoot] { return existing }
        let display = displayNames[canonicalRoot] ?? baseName(canonicalRoot)
        let cache = loadLinkCache(canonicalRoot: canonicalRoot, displayName: display)
        caches[canonicalRoot] = cache
        return cache
    }
}
