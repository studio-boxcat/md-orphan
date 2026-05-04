import Darwin
import Foundation

public struct GlobalConfig: Equatable {
    public let repos: [String: String]
    public init(repos: [String: String]) { self.repos = repos }
}

public enum ConfigError: Error, CustomStringConvertible {
    case malformedJSON(path: String, message: String)
    case missingReposKey(path: String)
    case repoPathExpansion(name: String, raw: String, missingVar: String)

    public var description: String {
        switch self {
        case .malformedJSON(let p, let m): return "\(p): malformed JSON: \(m)"
        case .missingReposKey(let p): return "\(p): missing 'repos' object"
        case .repoPathExpansion(let n, let r, let v):
            return "repo '\(n)' path '\(r)': undefined env var $\(v)"
        }
    }
}

/// Default global config path: $XDG_CONFIG_HOME/md-orphan/md-orphan.json,
/// falling back to $HOME/.config/md-orphan/md-orphan.json.
public func defaultConfigPath() -> String {
    let xdg = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"]
    let base = (xdg?.isEmpty == false ? xdg! : (homeDir() + "/.config"))
    return base + "/md-orphan/md-orphan.json"
}

public func homeDir() -> String {
    if let h = ProcessInfo.processInfo.environment["HOME"], !h.isEmpty { return h }
    return NSHomeDirectory()
}

/// Load the global config. Returns empty config if path doesn't exist (cross-repo features just don't fire).
public func loadGlobalConfig(path: String) throws -> GlobalConfig {
    guard FileManager.default.fileExists(atPath: path) else {
        return GlobalConfig(repos: [:])
    }
    let data: Data
    do { data = try Data(contentsOf: URL(fileURLWithPath: path)) }
    catch { throw ConfigError.malformedJSON(path: path, message: error.localizedDescription) }

    let obj: Any
    do { obj = try JSONSerialization.jsonObject(with: data) }
    catch { throw ConfigError.malformedJSON(path: path, message: error.localizedDescription) }

    guard let dict = obj as? [String: Any] else {
        throw ConfigError.malformedJSON(path: path, message: "top-level must be an object")
    }
    // Accept either the wrapped form `{"repos": {...}}` or a flat `{name: path, ...}` map.
    let reposRaw: [String: String]
    if let wrapped = dict["repos"] as? [String: String] {
        reposRaw = wrapped
    } else if dict["repos"] != nil {
        throw ConfigError.malformedJSON(path: path, message: "'repos' must be a {string:string} map")
    } else if let flat = obj as? [String: String] {
        reposRaw = flat
    } else {
        throw ConfigError.malformedJSON(
            path: path,
            message: "expected {\"repos\": {...}} or a flat {name: path} map"
        )
    }

    var repos: [String: String] = [:]
    for (name, raw) in reposRaw {
        repos[name] = try expandPath(raw, repoName: name)
    }
    return GlobalConfig(repos: repos)
}

/// Expand leading `~/` and any `$VAR` / `${VAR}` references against the environment.
/// Throws if a referenced variable is undefined.
public func expandPath(_ raw: String, repoName: String) throws -> String {
    var s = raw
    if s == "~" { s = homeDir() }
    else if s.hasPrefix("~/") { s = homeDir() + String(s.dropFirst(1)) }

    var result = ""
    var i = s.startIndex
    while i < s.endIndex {
        let c = s[i]
        if c != "$" { result.append(c); i = s.index(after: i); continue }

        let after = s.index(after: i)
        guard after < s.endIndex else { result.append(c); break }
        var nameStart = after
        var nameEnd = after
        let braced = s[after] == "{"
        if braced { nameStart = s.index(after: after) }
        var j = nameStart
        while j < s.endIndex {
            let ch = s[j]
            if braced { if ch == "}" { break } }
            else if !(ch.isLetter || ch.isNumber || ch == "_") { break }
            j = s.index(after: j)
        }
        nameEnd = j
        let name = String(s[nameStart..<nameEnd])
        if name.isEmpty { result.append(c); i = after; continue }
        guard let val = ProcessInfo.processInfo.environment[name] else {
            throw ConfigError.repoPathExpansion(name: repoName, raw: raw, missingVar: name)
        }
        result.append(val)
        i = braced && nameEnd < s.endIndex ? s.index(after: nameEnd) : nameEnd
    }
    return result
}

/// Built-in default excludes — directories that are big, auto-generated, or never doc trees.
public let defaultExcludes: [String] = [
    ".git/",
    ".svn/",
    ".hg/",
    "node_modules/",
    ".build/",
    "DerivedData/",
    "Library/",
    "Pods/",
    "target/",
    "vendor/",
    ".venv/",
    "__pycache__/",
]

/// Read `<root>/.md-orphan`, return one pattern per line. `#` line-comments and blanks ignored.
/// Returns empty array if the file doesn't exist. Throws on read error.
public func loadProjectIgnore(root: String) throws -> [String] {
    let path = root + "/.md-orphan"
    guard FileManager.default.fileExists(atPath: path) else { return [] }
    let raw = try String(contentsOfFile: path, encoding: .utf8)
    var patterns: [String] = []
    for line in raw.split(separator: "\n", omittingEmptySubsequences: false) {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }
        patterns.append(trimmed)
    }
    return patterns
}
