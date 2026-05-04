import Darwin
import Foundation

public struct RepoIndex: Equatable {
    public let root: String                  // canonical (realpath'd) absolute root
    public let mdFiles: Set<String>           // relPaths of .md files (orphan reachability set)
    public let byName: [String: [String]]    // basename → [absPath]; .md only by default
    public let exclude: [String]             // effective patterns applied

    public init(
        root: String,
        mdFiles: Set<String>,
        byName: [String: [String]],
        exclude: [String]
    ) {
        self.root = root
        self.mdFiles = mdFiles
        self.byName = byName
        self.exclude = exclude
    }
}

/// Walk a repo once and produce both the .md inode map (for orphan reachability) and the
/// basename → absolute-path map for style/uniqueness checks.
///
/// By default `byName` only includes `.md` files — that's all the style/fallback paths consult,
/// and including every file in a Unity/Xcode repo dominates runtime (>1s on a 100k-file tree).
/// Pass `includeAllExtensions: true` for the rare case of style-checking non-`.md` references.
///
/// `exclude` is composed with `defaultExcludes` unless `useDefaultExcludes` is false.
public func indexRepo(
    root: String,
    exclude: [String] = [],
    useDefaultExcludes: Bool = true,
    includeAllExtensions: Bool = false
) -> RepoIndex {
    let resolved = realPath(root) ?? root
    let effective = (useDefaultExcludes ? defaultExcludes : []) + exclude

    let rootCStr = strdup(resolved)!
    defer { free(rootCStr) }
    var argv: [UnsafeMutablePointer<CChar>?] = [rootCStr, nil]

    guard let stream = fts_open(&argv, FTS_PHYSICAL | FTS_NOCHDIR | FTS_NOSTAT, nil) else {
        return RepoIndex(root: resolved, mdFiles: [], byName: [:], exclude: effective)
    }
    defer { fts_close(stream) }

    let rootLen = resolved.utf8.count
    var mdFiles: Set<String> = []
    var byName: [String: [String]] = [:]

    while let entry = fts_read(stream) {
        let info = Int32(entry.pointee.fts_info)
        let nameLen = Int(entry.pointee.fts_namelen)
        let namePtr = entry.pointee.fts_path
            .advanced(by: Int(entry.pointee.fts_pathlen) - nameLen)

        if info == FTS_D {
            // Skip dot-dirs at any depth (.git, .build, .venv, .config, ...)
            if nameLen > 1 && namePtr.pointee == 0x2E {
                fts_set(stream, entry, FTS_SKIP)
                continue
            }
            if !effective.isEmpty {
                let absPath = String(cString: entry.pointee.fts_path)
                let relPath = absPath.utf8.count > rootLen + 1
                    ? String(absPath.dropFirst(rootLen + 1))
                    : ""
                if !relPath.isEmpty && isExcluded(relPath, by: effective) {
                    fts_set(stream, entry, FTS_SKIP)
                }
            }
            continue
        }

        guard info == FTS_F || info == FTS_SL || info == FTS_NSOK else { continue }

        let bytes = UnsafeRawBufferPointer(start: namePtr, count: nameLen).bindMemory(to: UInt8.self)
        let isMd = nameLen >= 3
            && bytes[nameLen - 3] == 0x2E
            && bytes[nameLen - 2] == 0x6D
            && bytes[nameLen - 1] == 0x64

        // Fast path: skip non-.md files entirely when we don't need them in byName.
        // Avoids the per-file String allocations that dominate runtime on repos with
        // ~100k non-.md files (Unity assets, codegen, etc.).
        if !isMd && !includeAllExtensions { continue }

        let absPath = String(cString: entry.pointee.fts_path)
        let relPath = absPath.utf8.count > rootLen + 1
            ? String(absPath.dropFirst(rootLen + 1))
            : absPath

        if !effective.isEmpty && isExcluded(relPath, by: effective) { continue }

        let basename = String(decoding: bytes, as: UTF8.self)
        byName[basename, default: []].append(absPath)

        if isMd {
            mdFiles.insert(relPath)
        }
    }

    return RepoIndex(root: resolved, mdFiles: mdFiles, byName: byName, exclude: effective)
}

