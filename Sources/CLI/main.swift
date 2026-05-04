import ArgumentParser
import Foundation
import MdOrphanLib

@main
struct MdOrphan: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "md-orphan",
        abstract: "Detect markdown files not reachable from entry points"
    )

    @Argument(help: "One or more markdown entry points")
    var entryPoints: [String] = []

    @Option(name: .long, help: "Exclude paths by prefix or glob; * and ? don't cross / (comma-separated, repeatable)")
    var exclude: [String] = []

    @Flag(name: [.long, .customShort("v")], help: "Show success message when all files are reachable")
    var verbose = false

    @Flag(name: .long, help: "Rewrite link style issues in place")
    var fix = false

    @Option(name: .long, help: "Path to global config (default: $XDG_CONFIG_HOME/md-orphan/md-orphan.json)")
    var config: String?

    @Flag(name: .long, help: "Disable built-in default excludes (.git, node_modules, Library, .build, ...)")
    var noDefaultExcludes = false

    @Flag(name: .long, help: "Disable per-file extraction cache")
    var noCache = false

    @Flag(name: .long, help: "Print the nearest CLAUDE.md (walks up from cwd): full path, blank line, contents")
    var claude = false

    func run() throws {
        if claude {
            try runClaude()
            return
        }
        guard !entryPoints.isEmpty else {
            throw ValidationError("missing entry point")
        }

        let resolvedEntries = try entryPoints.map { ep -> String in
            guard let abs = realPath(ep) else {
                throw ValidationError("\(ep): no such file")
            }
            return abs
        }

        let excludePatterns = exclude.flatMap { $0.split(separator: ",").map(String.init) }
        for p in excludePatterns {
            if p.contains("**") {
                throw ValidationError("--exclude: '**' globstar not supported; * matches within a single directory")
            }
            if p.contains("[") && !p.contains("]") {
                throw ValidationError("--exclude: unclosed '[' in pattern '\(p)'")
            }
        }

        let configPath = config ?? defaultConfigPath()
        let globalConfig: GlobalConfig
        do { globalConfig = try loadGlobalConfig(path: configPath) }
        catch let e as ConfigError { throw ValidationError("\(e)") }

        let root = dirName(resolvedEntries[0])
        let projectIgnore = (try? loadProjectIgnore(root: root)) ?? []
        let layeredExclude = excludePatterns + projectIgnore
        let effective = noDefaultExcludes ? layeredExclude : (defaultExcludes + layeredExclude)
        let allFiles = discoverFiles(root: root, exclude: effective)

        let crawlOptions = CrawlOptions(
            repos: globalConfig.repos,
            useDefaultExcludes: !noDefaultExcludes,
            extraExcludes: excludePatterns
        )
        let cache = ExtractionCache(enabled: !noCache)
        let (reachable, issues) = bfsCrawl(
            entryPaths: resolvedEntries, root: root,
            allFiles: allFiles, options: crawlOptions, cache: cache
        )
        defer { cache.save() }

        let orphans = allFiles
            .filter { !reachable.contains($0.key) }
            .map(\.value)
            .sorted()

        let names = entryPoints.map { baseName($0) }.joined(separator: ", ")
        let relSource = { (issue: LinkIssue) -> String in
            issue.source.hasPrefix(root + "/") ? String(issue.source.dropFirst(root.count + 1)) : issue.source
        }

        var failed = false

        let broken = issues.filter { $0.kind == .broken }
        let ambiguous = issues.filter { if case .ambiguous = $0.kind { return true }; return false }
        let brokenAnchors = issues.filter { if case .brokenAnchor = $0.kind { return true }; return false }
        let unknownRepos = issues.filter { if case .unknownRepo = $0.kind { return true }; return false }
        let crossRepoBroken = issues.filter { if case .crossRepoBroken = $0.kind { return true }; return false }
        let styleIssues = issues.filter { if case .style = $0.kind { return true }; return false }

        if !broken.isEmpty {
            print("\u{1F517} \(broken.count) broken links:")
            for b in broken {
                print("  \(b.link) in \(relSource(b))")
            }
            failed = true
        }

        if !ambiguous.isEmpty {
            print("\u{26A0}\u{FE0F} \(ambiguous.count) ambiguous links:")
            for a in ambiguous {
                if case .ambiguous(let count) = a.kind {
                    print("  \(a.link) in \(relSource(a)) (\(count) files match)")
                }
            }
            failed = true
        }

        if !brokenAnchors.isEmpty {
            print("\u{2693} \(brokenAnchors.count) broken anchors:")
            for a in brokenAnchors {
                if case .brokenAnchor(let frag) = a.kind {
                    print("  \(a.link)#\(frag) in \(relSource(a))")
                }
            }
            failed = true
        }

        if !unknownRepos.isEmpty {
            print("\u{1F6AB} \(unknownRepos.count) unknown cross-repo references:")
            for u in unknownRepos {
                if case .unknownRepo(let r) = u.kind {
                    print("  `\(u.link)` (\(r)) in \(relSource(u))")
                }
            }
            failed = true
        }

        if !crossRepoBroken.isEmpty {
            print("\u{1F517} \(crossRepoBroken.count) broken cross-repo links:")
            for b in crossRepoBroken {
                if case .crossRepoBroken(let r) = b.kind {
                    print("  `\(b.link)` (\(r)) in \(relSource(b))")
                }
            }
            failed = true
        }

        if !styleIssues.isEmpty {
            let header = fix ? "fixed" : "issues"
            print("\u{1F4DD} \(styleIssues.count) link style \(header):")
            let sorted = styleIssues.sorted {
                if $0.source != $1.source { return relSource($0) < relSource($1) }
                guard case .style(_, _, let a, _) = $0.kind,
                      case .style(_, _, let b, _) = $1.kind else { return false }
                return a < b
            }
            for issue in sorted {
                guard case .style(let scope, let suggested, _, _) = issue.kind else { continue }
                let (lhs, rhs) = renderStyle(link: issue.link, suggested: suggested, scope: scope)
                print("  \(relSource(issue)): \(lhs) -> \(rhs)")
            }
            if fix {
                applyStyleFixes(styleIssues)
            } else {
                print("  (run with --fix to apply)")
                failed = true
            }
        }

        if !orphans.isEmpty {
            print("\u{274C} \(orphans.count) orphan markdown files (not reachable from \(names)):")
            for path in orphans {
                print("  \(path)")
            }
            failed = true
        }

        if failed {
            throw ExitCode(1)
        } else if verbose {
            print("\u{2705} All \(allFiles.count) markdown files are reachable from \(names)")
        }
    }
}

/// Walk up from cwd until a CLAUDE.md (or AGENTS.md fallback — they're often symlinked together)
/// is found. Print its absolute canonical path on the first line, a blank line, then its contents.
/// Exit non-zero if nothing is found between cwd and `/`.
extension MdOrphan {
    fileprivate func runClaude() throws {
        let cwd = FileManager.default.currentDirectoryPath
        var dir = cwd
        while !dir.isEmpty, dir != "/" {
            for name in ["CLAUDE.md", "AGENTS.md"] {
                let candidate = dir + "/" + name
                if FileManager.default.fileExists(atPath: candidate) {
                    let canonical = realPath(candidate) ?? candidate
                    print(canonical)
                    print()
                    if let data = try? Data(contentsOf: URL(fileURLWithPath: canonical)),
                       let text = String(data: data, encoding: .utf8) {
                        print(text, terminator: text.hasSuffix("\n") ? "" : "\n")
                    }
                    return
                }
            }
            guard let slash = dir.lastIndex(of: "/") else { break }
            dir = String(dir[..<slash])
        }
        fputs("md-orphan --claude: no CLAUDE.md or AGENTS.md found from \(cwd) up to /\n", stderr)
        throw ExitCode(1)
    }
}

private func renderStyle(link: String, suggested: String, scope: LinkIssue.StyleScope) -> (String, String) {
    switch scope {
    case .wiki:
        return ("[[\(link)]]", "[[\(suggested)]]")
    case .crossRepo(let repo):
        return ("`\(link)` (\(repo))", "`\(suggested)` (\(repo))")
    }
}

/// Apply style fixes by rewriting just the path bytes inside [[...]] or `...`.
/// Replacements applied per file in descending offset order so earlier offsets stay valid.
/// Atomic write (.atomicWrite) so a crash mid-write can't truncate the source.
private func applyStyleFixes(_ issues: [LinkIssue]) {
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
