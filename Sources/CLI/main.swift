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
        let (reachable, broken) = bfsCrawl(entryPaths: resolvedEntries, root: root, allFiles: allFiles)

        let orphans = allFiles
            .filter { !reachable.contains($0.key) }
            .map(\.value)
            .sorted()

        let names = entryPoints.map { path -> String in
            if let idx = path.lastIndex(of: "/") {
                return String(path[path.index(after: idx)...])
            }
            return path
        }.joined(separator: ", ")

        var failed = false

        if !broken.isEmpty {
            let relBroken = broken.map { b in
                let src = b.source.hasPrefix(root + "/") ? String(b.source.dropFirst(root.count + 1)) : b.source
                return (link: b.link, source: src)
            }
            print("\u{1F517} \(broken.count) broken links:")
            for b in relBroken {
                print("  \(b.link) in \(b.source)")
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
