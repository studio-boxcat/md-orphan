import ArgumentParser
import MdOrphanLib

@main
struct MdOrphan: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "md-orphan",
        abstract: "Detect markdown files not reachable from entry points"
    )

    @Argument(help: "One or more markdown entry points")
    var entryPoints: [String]

    @Option(name: .long, help: "Exclude paths by prefix or glob; * and ? don't cross / (comma-separated, repeatable)")
    var exclude: [String] = []

    @Flag(name: [.long, .customShort("v")], help: "Show success message when all files are reachable")
    var verbose = false

    func run() throws {
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
        let root = dirName(resolvedEntries[0])
        let allFiles = discoverFiles(root: root, exclude: excludePatterns)
        let (reachable, issues) = bfsCrawl(entryPaths: resolvedEntries, root: root, allFiles: allFiles)

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
