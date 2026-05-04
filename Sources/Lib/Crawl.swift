import Darwin

// MARK: - Public types

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

// MARK: - Resolution helper (used by both same-repo and tests)

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

// MARK: - bfsCrawl

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

// MARK: - CrawlState

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

    mutating func resolve(_ link: Link, source: BfsQueueItem) {
        switch link.kind {
        case .wiki, .standard:
            resolveSameRepo(link, source: source)
        case .crossRepo(let repoName):
            resolveCrossRepo(link, source: source, repoName: repoName)
        }
    }

    private mutating func resolveSameRepo(_ link: Link, source: BfsQueueItem) {
        let current = indices[source.repoRoot] ?? entryIndex
        guard let resolved = resolveLink(link.target, relativeTo: source.path, root: current.root) else {
            return
        }
        let isMd = link.target.hasSuffix(".md")
        var canonical = realPath(resolved)

        // Basename fallback only for .md links.
        if canonical == nil && isMd {
            let basename = baseName(link.target)
            if let candidates = current.byName[basename], candidates.count == 1 {
                canonical = realPath(candidates[0])
            } else if let candidates = current.byName[basename], candidates.count > 1 {
                issues.append(LinkIssue(link: link.target, source: source.path,
                                        kind: .ambiguous(candidates.count)))
                return
            }
        }
        guard let canonical else {
            issues.append(LinkIssue(link: link.target, source: source.path, kind: .broken))
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
            issues.append(LinkIssue(link: link.target, source: source.path,
                                    kind: .brokenAnchor(fragment)))
        }

        // Enqueue: usually same-repo entry-repo dedup; cross-repo if symlink crosses boundary.
        let owningRoot = repoRootContaining(canonical) ?? current.root
        enqueueResolved(canonical: canonical, owningRoot: owningRoot)
    }

    private mutating func resolveCrossRepo(_ link: Link, source: BfsQueueItem, repoName: String) {
        guard let repoRoot = resolvedRepos[repoName] else {
            issues.append(LinkIssue(link: link.target, source: source.path,
                                    kind: .unknownRepo(repo: repoName)))
            return
        }

        // Cross-repo paths are repo-root-relative; normalize ./ and ../ but reject root escape.
        let rawCombined = link.target.hasPrefix("/") ? repoRoot + link.target : repoRoot + "/" + link.target
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

        let isMd = link.target.hasSuffix(".md")
        var canonical: String? = withinRoot ? realPath(normalized) : nil
        let repoIndex = indexFor(repoRoot)

        // Basename fallback in target repo — also handles `../` escapes for style violations.
        if canonical == nil && isMd {
            let basename = baseName(link.target)
            if let candidates = repoIndex.byName[basename], candidates.count == 1 {
                canonical = realPath(candidates[0])
            } else if let candidates = repoIndex.byName[basename], candidates.count > 1 {
                issues.append(LinkIssue(link: link.target, source: source.path,
                                        kind: .ambiguous(candidates.count)))
                return
            }
        }
        guard let canonical else {
            issues.append(LinkIssue(link: link.target, source: source.path,
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
            issues.append(LinkIssue(link: link.target, source: source.path,
                                    kind: .brokenAnchor(fragment)))
        }

        // Self-ref into entry repo (rare) funnels into entry-repo dedup.
        let owningRoot = (canonical == entryRoot || canonical.hasPrefix(entryRoot + "/"))
            ? entryRoot
            : repoRoot
        enqueueResolved(canonical: canonical, owningRoot: owningRoot)
    }

    private mutating func emitStyleIfNeeded(
        link: Link, source: String, canonical: String,
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
        guard link.target != canonicalForm else { return }
        issues.append(LinkIssue(
            link: link.target, source: source,
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
