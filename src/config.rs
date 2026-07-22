//! Global config (`~/.config/md-orphan/md-orphan.json`) + per-repo `.md-orphan` ignore file.
//!
//! Mirrors `Sources/Lib/Config.swift`. Custom `Deserialize` for `GlobalConfig` accepts both
//! wrapped (`{"repos": {...}}`) and flat (`{name: path}`) JSON shapes for back-compat with
//! Swift-era configs.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: cannot read: {message}")]
    Unreadable { path: String, message: String },
    #[error("{path}: malformed JSON: {message}")]
    MalformedJson { path: String, message: String },
    #[error("repo '{name}' path '{raw}': undefined env var ${missing_var}")]
    RepoPathExpansion {
        name: String,
        raw: String,
        missing_var: String,
    },
    #[error("repo '{name}' path '{raw}': unclosed `${{` brace expansion")]
    UnclosedBrace { name: String, raw: String },
    #[error("repo '{name}' path '{raw}': unknown user in `~{user}/` expansion")]
    UnknownUser {
        name: String,
        raw: String,
        user: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobalConfig {
    pub repos: HashMap<String, String>, // resolved (env-expanded) absolute paths
}

impl GlobalConfig {
    /// Drop entries whose path isn't a directory on this machine. Cross-repo refs to a
    /// configured-but-not-cloned repo would otherwise become spurious "broken" errors;
    /// after retain, the parser drops them as plain inline code.
    pub fn retain_existing(&mut self) {
        self.repos.retain(|_, path| Path::new(path).is_dir());
    }
}

/// Default global config path: `$XDG_CONFIG_HOME/md-orphan/md-orphan.json`, fallback to
/// `$HOME/.config/md-orphan/md-orphan.json`.
pub fn default_config_path() -> PathBuf {
    PathBuf::from(format!("{}/md-orphan/md-orphan.json", xdg_config_home()))
}

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config`. Shared base for the global config
/// (here) and both cache layers (`cache.rs`).
pub(crate) fn xdg_config_home() -> String {
    env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", home_dir()))
}

/// Current user's home directory: `$HOME`, or `""` when unset (no passwd fallback —
/// `$HOME` is always set in any environment this tool runs in).
pub(crate) fn home_dir() -> String {
    env::var("HOME").unwrap_or_default()
}

/// Another user's home directory via `getpwnam(3)`. `None` for unknown users.
fn user_home_dir(user: &str) -> Option<String> {
    let c = std::ffi::CString::new(user).ok()?;
    // SAFETY: getpwnam returns a pointer into static storage; config loading is
    // single-threaded and the result is copied out before any other passwd call.
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned())
    }
}

/// Load the global config. Returns empty config if path doesn't exist (cross-repo features
/// just don't fire). Errors only on malformed JSON or undefined env-var references.
pub fn load_global_config(path: &Path) -> Result<GlobalConfig, ConfigError> {
    if !path.exists() {
        return Ok(GlobalConfig::default());
    }
    let raw = fs::read_to_string(path).map_err(|e| ConfigError::Unreadable {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| ConfigError::MalformedJson {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let repos_raw = extract_repos(&value, path)?;
    let mut repos: HashMap<String, String> = HashMap::new();
    for (name, raw_path) in repos_raw {
        repos.insert(name.clone(), expand_path(&raw_path, &name)?);
    }
    Ok(GlobalConfig { repos })
}

/// Accept both `{"repos": {...}}` and flat `{name: path}` JSON shapes.
fn extract_repos(value: &serde_json::Value, path: &Path) -> Result<HashMap<String, String>, ConfigError> {
    let obj = value.as_object().ok_or_else(|| ConfigError::MalformedJson {
        path: path.display().to_string(),
        message: "top-level must be an object".into(),
    })?;
    if let Some(repos_val) = obj.get("repos") {
        let repos_obj = repos_val.as_object().ok_or_else(|| ConfigError::MalformedJson {
            path: path.display().to_string(),
            message: "'repos' must be a {string:string} map".into(),
        })?;
        return strings_only(repos_obj, path);
    }
    // Flat form: every value must be a string.
    if obj.values().all(|v| v.is_string()) {
        return strings_only(obj, path);
    }
    Err(ConfigError::MalformedJson {
        path: path.display().to_string(),
        message: "expected {\"repos\": {...}} or a flat {name: path} map".into(),
    })
}

fn strings_only(
    obj: &serde_json::Map<String, serde_json::Value>,
    path: &Path,
) -> Result<HashMap<String, String>, ConfigError> {
    let mut out = HashMap::new();
    for (k, v) in obj {
        let s = v.as_str().ok_or_else(|| ConfigError::MalformedJson {
            path: path.display().to_string(),
            message: format!("value for '{k}' must be a string"),
        })?;
        out.insert(k.clone(), s.to_string());
    }
    Ok(out)
}

/// Expand leading `~/` (or `~user/`) and any `$VAR` / `${VAR}` references. Throws if a
/// referenced env var or user is undefined.
pub fn expand_path(raw: &str, repo_name: &str) -> Result<String, ConfigError> {
    let s = if raw == "~" {
        home_dir()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{}/{}", home_dir(), rest)
    } else if let Some(after) = raw.strip_prefix('~') {
        // `~user` / `~user/rest` — other-user home via getpwnam(3).
        let (user, rest) = match after.find('/') {
            Some(i) => (&after[..i], &after[i..]),
            None => (after, ""),
        };
        let home = user_home_dir(user).ok_or_else(|| ConfigError::UnknownUser {
            name: repo_name.to_string(),
            raw: raw.to_string(),
            user: user.to_string(),
        })?;
        format!("{home}{rest}")
    } else {
        raw.to_string()
    };

    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy through to the next `$` or end. `i` only ever stops on the ASCII `$` or at
            // len, so the slice boundaries are always char boundaries — a panic here would be
            // a scanner logic bug, not a data condition.
            let start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            out.push_str(&s[start..i]);
            continue;
        }
        let after = i + 1;
        if after >= bytes.len() {
            out.push('$');
            break;
        }
        let braced = bytes[after] == b'{';
        let name_start = if braced { after + 1 } else { after };
        let mut j = name_start;
        while j < bytes.len() {
            let c = bytes[j];
            if braced {
                if c == b'}' {
                    break;
                }
            } else if !(c.is_ascii_alphanumeric() || c == b'_') {
                break;
            }
            j += 1;
        }
        if braced && (j >= bytes.len() || bytes[j] != b'}') {
            return Err(ConfigError::UnclosedBrace {
                name: repo_name.to_string(),
                raw: raw.to_string(),
            });
        }
        // `name_start`/`j` bound an ASCII-only var name — always char boundaries.
        let name = &s[name_start..j];
        if name.is_empty() {
            out.push('$');
            i = after;
            continue;
        }
        let val = env::var(name).map_err(|_| ConfigError::RepoPathExpansion {
            name: repo_name.to_string(),
            raw: raw.to_string(),
            missing_var: name.to_string(),
        })?;
        out.push_str(&val);
        i = if braced { j + 1 } else { j };
    }
    Ok(out)
}

// MARK: - Per-repo .md-orphan

/// True iff `<root>/.md-orphan` exists.
pub fn project_ignore_exists(root: &Path) -> bool {
    root.join(".md-orphan").exists()
}

/// Walk up from `start`'s parent looking for the nearest ancestor containing `.md-orphan`.
/// Returns `None` if none found before the filesystem root.
pub fn find_project_ignore_ancestor(start: &Path) -> Option<PathBuf> {
    let mut cur = start.parent();
    while let Some(dir) = cur {
        if dir.join(".md-orphan").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Read `<root>/.md-orphan`, return one pattern per line. `#` line-comments + blanks ignored.
pub fn load_project_ignore(root: &Path) -> std::io::Result<Vec<String>> {
    let path = root.join(".md-orphan");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                None
            } else {
                Some(t.to_string())
            }
        })
        .collect())
}

/// [`load_project_ignore`], but an existing-yet-unreadable `.md-orphan` warns to stderr and
/// yields no patterns instead of being silently swallowed. A missing file is still `vec![]`.
pub fn project_ignore_or_warn(root: &Path) -> Vec<String> {
    load_project_ignore(root).unwrap_or_else(|e| {
        eprintln!(
            "md-orphan: warning: cannot read {}/.md-orphan: {e}; ignoring its patterns",
            root.display()
        );
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn expand_path_home_and_env() {
        // SAFETY: tests are single-threaded by default in cargo, but env::set_var is unsafe in
        // multi-threaded contexts. Our tests don't share env vars.
        unsafe { env::set_var("MD_ORPHAN_TEST_X", "/tmp/something"); }
        let result = expand_path("$MD_ORPHAN_TEST_X/sub", "test").unwrap();
        assert_eq!(result, "/tmp/something/sub");
        let braced = expand_path("${MD_ORPHAN_TEST_X}/sub", "test").unwrap();
        assert_eq!(braced, "/tmp/something/sub");
        unsafe { env::remove_var("MD_ORPHAN_TEST_X"); }
    }

    #[test]
    fn expand_path_missing_var_throws() {
        let r = expand_path("$NOPE_NOT_DEFINED_VAR/x", "r");
        assert!(matches!(r, Err(ConfigError::RepoPathExpansion { .. })));
    }

    #[test]
    fn expand_path_unclosed_brace_throws() {
        unsafe { env::set_var("MD_ORPHAN_RT_X", "/x"); }
        let r = expand_path("${MD_ORPHAN_RT_X/sub", "r");
        assert!(matches!(r, Err(ConfigError::UnclosedBrace { .. })));
        unsafe { env::remove_var("MD_ORPHAN_RT_X"); }
    }

    #[test]
    fn expand_path_tilde_user() {
        // `~<current user>/x` must expand to the same home as `~/x` (pw_dir == $HOME in any
        // non-exotic setup). Skip when $USER is unavailable.
        let Ok(user) = env::var("USER") else { return };
        let expanded = expand_path(&format!("~{user}/x"), "r").unwrap();
        assert_eq!(expanded, format!("{}/x", home_dir()));
        // Bare `~user` without a trailing path expands to the home itself.
        let bare = expand_path(&format!("~{user}"), "r").unwrap();
        assert_eq!(bare, home_dir());
    }

    #[test]
    fn expand_path_unknown_user_throws() {
        let r = expand_path("~no_such_user_zzz/x", "r");
        assert!(matches!(r, Err(ConfigError::UnknownUser { .. })), "got: {r:?}");
    }

    #[test]
    fn load_project_ignore_parses_lines() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".md-orphan"), "# comment\nLibrary/\n\ndocs/draft-*.md\n").unwrap();
        let patterns = load_project_ignore(dir.path()).unwrap();
        assert_eq!(patterns, vec!["Library/", "docs/draft-*.md"]);
    }

    #[test]
    fn load_project_ignore_returns_empty_when_absent() {
        let dir = TempDir::new().unwrap();
        let patterns = load_project_ignore(dir.path()).unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn project_ignore_exists_distinguishes_absent_from_empty() {
        let dir = TempDir::new().unwrap();
        assert!(!project_ignore_exists(dir.path()));
        fs::write(dir.path().join(".md-orphan"), "").unwrap();
        assert!(project_ignore_exists(dir.path()));
    }

    #[test]
    fn find_project_ignore_ancestor_walks_strictly_upward() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".md-orphan"), "").unwrap();
        let nested = root.join("docs").join("specs");
        fs::create_dir_all(&nested).unwrap();

        // Strict ancestor: parent of `nested` has none, but its parent does.
        let found = find_project_ignore_ancestor(&nested).unwrap();
        assert_eq!(fs::canonicalize(&found).unwrap(), fs::canonicalize(root).unwrap());

        // `start` itself is not checked — only ancestors above it.
        assert!(find_project_ignore_ancestor(root).is_none());
    }

    #[test]
    fn find_project_ignore_ancestor_none_when_no_match() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        assert!(find_project_ignore_ancestor(&nested).is_none());
    }

    #[test]
    fn find_project_ignore_ancestor_returns_nearest() {
        let dir = TempDir::new().unwrap();
        let mid = dir.path().join("mid");
        let leaf = mid.join("leaf");
        fs::create_dir_all(&leaf).unwrap();
        fs::write(dir.path().join(".md-orphan"), "").unwrap();
        fs::write(mid.join(".md-orphan"), "").unwrap();

        let found = find_project_ignore_ancestor(&leaf).unwrap();
        assert_eq!(fs::canonicalize(&found).unwrap(), fs::canonicalize(&mid).unwrap());
    }

    #[test]
    fn load_global_config_wrapped_form() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"repos":{"foo":"/tmp/foo","bar":"/tmp/bar"}}"#).unwrap();
        let cfg = load_global_config(&path).unwrap();
        assert_eq!(cfg.repos.get("foo"), Some(&"/tmp/foo".to_string()));
        assert_eq!(cfg.repos.get("bar"), Some(&"/tmp/bar".to_string()));
    }

    #[test]
    fn load_global_config_flat_form() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, r#"{"foo":"/tmp/foo","bar":"/tmp/bar"}"#).unwrap();
        let cfg = load_global_config(&path).unwrap();
        assert_eq!(cfg.repos.len(), 2);
    }

    #[test]
    fn load_global_config_missing_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.json");
        let cfg = load_global_config(&path).unwrap();
        assert!(cfg.repos.is_empty());
    }

    #[test]
    fn retain_existing_drops_missing_paths() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let mut cfg = GlobalConfig {
            repos: HashMap::from([
                ("present".to_string(), real.to_string_lossy().to_string()),
                ("absent".to_string(), "/tmp/__md_orphan_nope_no_such_dir__".to_string()),
            ]),
        };
        cfg.retain_existing();
        assert!(cfg.repos.contains_key("present"));
        assert!(!cfg.repos.contains_key("absent"));
    }

    #[test]
    fn retain_existing_drops_path_that_is_a_file_not_a_dir() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("not-a-dir");
        fs::write(&file, "").unwrap();
        let mut cfg = GlobalConfig {
            repos: HashMap::from([
                ("file-not-dir".to_string(), file.to_string_lossy().to_string()),
            ]),
        };
        cfg.retain_existing();
        assert!(cfg.repos.is_empty());
    }
}
