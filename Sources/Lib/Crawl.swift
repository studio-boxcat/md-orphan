import Darwin
import Foundation

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
/// Visited tracking is split by repo scope, both keyed by canonical absolute path:
///  - **Entry repo**: `reachable` set. Used for orphan detection.
///  - **Cross-repo**: `crossRepoVisited` set. Cross-repo files are verified + style-checked
///    but never enter `reachable` (orphan detection is scoped to the entry repo).
///
/// Symlinks pointing to the same `.md` from two paths resolve to one canonical via `realPath`,
/// so path-based dedup handles them. Hardlinks (rare in doc trees) are processed twice.
///
/// Single-threaded — TaskGroup overhead exceeds I/O at this scale: https://forums.swift.org/t/taskgroup-and-parallelism/51039
public func bfsCrawl(
    entryPaths: [String],
    index: RepoIndex,
    options: CrawlOptions = .init(),
    cache: ExtractionCache = ExtractionCache(enabled: false)
) -> (reachable: Set<String>, issues: [LinkIssue]) {
    var state = CrawlState(entryIndex: index, options: options, cache: cache)
    state.seed(entryPaths)
    while let item = state.dequeue() {
        state.visit(item)
    }
    state.pruneCache()
    return (state.reachable, state.issues)
}

/// Convenience: walk the repo and crawl in one call. Equivalent to `indexRepo` + `bfsCrawl`.
public func bfsCrawl(
    entryPaths: [String],
    root: String,
    options: CrawlOptions = .init(),
    cache: ExtractionCache = ExtractionCache(enabled: false)
) -> (index: RepoIndex, reachable: Set<String>, issues: [LinkIssue]) {
    let projectIgnore = (try? loadProjectIgnore(root: root)) ?? []
    let exclude = options.extraExcludes + projectIgnore
    let idx = indexRepo(root: root, exclude: exclude, useDefaultExcludes: options.useDefaultExcludes)
    let (reachable, issues) = bfsCrawl(entryPaths: entryPaths, index: idx,
                                       options: options, cache: cache)
    return (idx, reachable, issues)
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
    var reachable: Set<String> = []                         // canonical paths visited in entry repo
    var issues: [LinkIssue] = []
    var headingCache: [String: Set<String>] = [:]

    init(entryIndex: RepoIndex, options: CrawlOptions, cache: ExtractionCache) {
        // entryIndex.root is already canonical (realpath'd inside indexRepo) — no further
        // canonicalization needed here. Mixing resolved/unresolved paths breaks prefix checks
        // (e.g. macOS /var/folders → /private/var/folders symlink).
        self.entryRoot = entryIndex.root
        self.entryIndex = entryIndex
        self.options = options
        self.cache = cache

        var resolvedRepos: [String: String] = [:]
        for (name, raw) in options.repos {
            resolvedRepos[name] = realPath(raw) ?? raw
        }
        self.resolvedRepos = resolvedRepos
        self.indices = [entryIndex.root: entryIndex]
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
            guard reachable.insert(item.path).inserted else { return }
        } else {
            guard crossRepoVisited.insert(item.path).inserted else { return }
        }
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
        // `source.repoRoot` was either the entry root (always indexed) or a cross-repo root
        // populated by `resolveCrossRepo` before the file was enqueued. `indexFor` is defensive:
        // re-indexes if the invariant ever breaks instead of silently using `entryIndex`.
        let current = indexFor(source.repoRoot)
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
        guard let relTarget = relPath(canonical, under: repoRoot) else { return }
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
            cache.prune(canonicalRoot: root, keepRelativePaths: idx.mdFiles)
        }
    }
}

// MARK: - --fix byte rewriter

/// Rewrite the path bytes inside `[[...]]` or `` `...` `` for every `.style` issue.
/// Replacements per source file are applied in descending byte-offset order so earlier offsets
/// stay valid. Atomic write (`.atomic` — Foundation's tmp + rename) so a crash mid-write can't
/// truncate the source. Failures are logged to stderr; non-style issues are ignored.
public func applyStyleFixes(_ issues: [LinkIssue]) {
    let bySource = Dictionary(grouping: issues, by: \.source)
    for (source, sourceIssues) in bySource {
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: source)) else {
            fputs("md-orphan: warning: cannot read \(source) for --fix\n", stderr)
            continue
        }
        var bytes = [UInt8](data)
        let sorted = sourceIssues.sorted {
            guard case .style(_, _, let a, _) = $0.kind,
                  case .style(_, _, let b, _) = $1.kind else { return false }
            return a > b
        }
        for issue in sorted {
            guard case .style(_, let suggested, let start, let end) = issue.kind else { continue }
            guard start <= end, end <= bytes.count else { continue }
            bytes.replaceSubrange(start..<end, with: Array(suggested.utf8))
        }
        do {
            try Data(bytes).write(to: URL(fileURLWithPath: source), options: .atomic)
        } catch {
            fputs("md-orphan: warning: cannot write \(source): \(error)\n", stderr)
        }
    }
}
