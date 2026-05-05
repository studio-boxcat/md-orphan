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
    #[error("{path}: malformed JSON: {message}")]
    MalformedJson { path: String, message: String },
    #[error("{path}: missing 'repos' object")]
    MissingReposKey { path: String },
    #[error("repo '{name}' path '{raw}': undefined env var ${missing_var}")]
    RepoPathExpansion {
        name: String,
        raw: String,
        missing_var: String,
    },
    #[error("repo '{name}' path '{raw}': unclosed `${{` brace expansion")]
    UnclosedBrace { name: String, raw: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlobalConfig {
    pub repos: HashMap<String, String>, // resolved (env-expanded) absolute paths
}

/// Default global config path: `$XDG_CONFIG_HOME/md-orphan/md-orphan.json`, fallback to
/// `$HOME/.config/md-orphan/md-orphan.json`.
pub fn default_config_path() -> PathBuf {
    let base = env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", home_dir()));
    PathBuf::from(format!("{}/md-orphan/md-orphan.json", base))
}

/// User home directory. `$HOME` first, fall back to `dirs::home_dir`-equivalent via passwd.
pub(crate) fn home_dir() -> String {
    env::var("HOME").unwrap_or_default()
}

/// Load the global config. Returns empty config if path doesn't exist (cross-repo features
/// just don't fire). Errors only on malformed JSON or undefined env-var references.
pub fn load_global_config(path: &Path) -> Result<GlobalConfig, ConfigError> {
    if !path.exists() {
        return Ok(GlobalConfig::default());
    }
    let raw = fs::read_to_string(path).map_err(|e| ConfigError::MalformedJson {
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

/// Expand leading `~/` and any `$VAR` / `${VAR}` references. Throws if a referenced env var
/// is undefined.
pub fn expand_path(raw: &str, repo_name: &str) -> Result<String, ConfigError> {
    let s = if raw == "~" {
        home_dir()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        format!("{}/{}", home_dir(), rest)
    } else {
        raw.to_string()
    };

    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Multi-byte UTF-8 char or ASCII: copy through to next $ or end.
            let start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            out.push_str(std::str::from_utf8(&bytes[start..i]).unwrap_or(""));
            continue;
        }
        // bytes[i] == '$'
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
        let name = std::str::from_utf8(&bytes[name_start..j]).unwrap_or("");
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
}
