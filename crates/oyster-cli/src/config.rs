use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing url: set --url or add 'url' to the active context")]
    MissingUrl,
    #[error("missing api_key: set --api-key or add 'api_key' to the active context")]
    MissingApiKey,
    #[error(
        "no active context; pass --context, set OYSTER_CONTEXT, or add 'active_context' to client.yaml"
    )]
    NoActiveContext,
    #[error("unknown context '{name}'; known contexts: {known}")]
    UnknownContext { name: String, known: String },
    #[error("no config file found; create ~/.config/oyster/client.yaml first")]
    NoConfigFile,
    #[error("failed to read config file {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write config file {path}: {source}")]
    WriteError {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    ParseError {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("failed to serialize config: {0}")]
    SerializeError(#[from] serde_yaml::Error),
    #[error(
        "config file {path} has insecure permissions {mode:o}; \
         refusing to read. Run: chmod 600 {path}"
    )]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("active context '{ctx}' has no apps; run `oyster app import <name>` first")]
    MissingApp { ctx: String },
    #[error("unknown app '{name}' in active context; known apps: {known}")]
    UnknownApp { name: String, known: String },
    #[error(
        "active context has multiple apps; pass --app <name> to disambiguate. Known apps: {known}"
    )]
    AmbiguousApp { known: String },
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_context: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, Context>,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, AppEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEntry {
    pub admin_key: String,
}

#[derive(Debug)]
pub struct ResolvedConfig {
    pub url: String,
    pub api_key: String,
}

/// Search for a config file using the standard search path.
/// If `explicit` is provided, only that path is checked.
pub fn find_config_file(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return if path.exists() {
            Some(path.to_owned())
        } else {
            None
        };
    }

    let candidates = [
        Some(PathBuf::from("client.yaml")),
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(|xdg| PathBuf::from(xdg).join("oyster/client.yaml")),
        dirs_fallback().map(|home| home.join(".config/oyster/client.yaml")),
    ];

    candidates
        .into_iter()
        .flatten()
        .find(|candidate| candidate.exists())
}

fn dirs_fallback() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Load a FileConfig from disk. Returns default if no config file is found.
/// Errors if an explicit path is given but cannot be read/parsed.
pub fn load_file_config(
    explicit: Option<&Path>,
) -> Result<(FileConfig, Option<PathBuf>), ConfigError> {
    let Some(path) = find_config_file(explicit) else {
        if let Some(explicit_path) = explicit {
            return Err(ConfigError::ReadError {
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
                path: explicit_path.to_owned(),
            });
        }
        return Ok((FileConfig::default(), None));
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&path).map_err(|e| ConfigError::ReadError {
            path: path.clone(),
            source: e,
        })?;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(ConfigError::InsecurePermissions {
                path: path.clone(),
                mode: mode & 0o7777,
            });
        }
    }

    let contents = std::fs::read_to_string(&path).map_err(|e| ConfigError::ReadError {
        path: path.clone(),
        source: e,
    })?;

    let config: FileConfig =
        serde_yaml::from_str(&contents).map_err(|e| ConfigError::ParseError {
            path: path.clone(),
            source: e,
        })?;

    Ok((config, Some(path)))
}

/// Resolve the active context name using the documented precedence:
/// 1) explicit `--context` flag, 2) `OYSTER_CONTEXT` env var, 3) the
///    `active_context` field in the file.
///
/// If none of those produce a name and the file has exactly one context,
/// that context is used. Otherwise returns `Ok(None)` — callers that
/// require a named context surface their own `NoActiveContext` error.
pub fn resolve_context_name(
    flag: Option<&str>,
    env: Option<&str>,
    file: &FileConfig,
) -> Result<Option<String>, ConfigError> {
    let candidate = flag
        .map(String::from)
        .or_else(|| env.map(String::from))
        .or_else(|| file.active_context.clone());

    if let Some(name) = candidate {
        if !file.contexts.contains_key(&name) {
            let known = file.contexts.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(ConfigError::UnknownContext { name, known });
        }
        return Ok(Some(name));
    }

    if file.contexts.len() == 1 {
        return Ok(file.contexts.keys().next().cloned());
    }

    Ok(None)
}

/// Resolve config for authenticated commands (requires both url and api_key).
pub fn resolve(
    explicit: Option<&Path>,
    cli_context: Option<&str>,
    cli_url: Option<&str>,
    cli_api_key: Option<&str>,
) -> Result<ResolvedConfig, ConfigError> {
    let (file, _config_path) = load_file_config(explicit)?;
    let env_ctx = std::env::var("OYSTER_CONTEXT").ok();
    let ctx_name = resolve_context_name(cli_context, env_ctx.as_deref(), &file)?;
    let ctx = ctx_name.as_ref().and_then(|n| file.contexts.get(n));

    let url = cli_url
        .map(String::from)
        .or_else(|| ctx.and_then(|c| c.url.clone()))
        .ok_or(ConfigError::MissingUrl)?;

    let api_key = cli_api_key
        .map(String::from)
        .or_else(|| ctx.and_then(|c| c.api_key.clone()))
        .ok_or(ConfigError::MissingApiKey)?;

    Ok(ResolvedConfig { url, api_key })
}

/// Resolve config for public commands (only requires url).
pub fn resolve_url_only(
    explicit: Option<&Path>,
    cli_context: Option<&str>,
    cli_url: Option<&str>,
) -> Result<(String, Option<PathBuf>), ConfigError> {
    let (file, config_path) = load_file_config(explicit)?;
    let env_ctx = std::env::var("OYSTER_CONTEXT").ok();
    let ctx_name = resolve_context_name(cli_context, env_ctx.as_deref(), &file)?;
    let ctx = ctx_name.as_ref().and_then(|n| file.contexts.get(n));

    let url = cli_url
        .map(String::from)
        .or_else(|| ctx.and_then(|c| c.url.clone()))
        .ok_or(ConfigError::MissingUrl)?;

    Ok((url, config_path))
}

/// Resolve a named context, erroring with `NoActiveContext` if none is
/// selected. Used by subcommands that operate on a specific context (e.g.
/// `app import`).
pub fn require_context_name(
    cli_context: Option<&str>,
    file: &FileConfig,
) -> Result<String, ConfigError> {
    let env_ctx = std::env::var("OYSTER_CONTEXT").ok();
    resolve_context_name(cli_context, env_ctx.as_deref(), file)?.ok_or(ConfigError::NoActiveContext)
}

/// Resolution bundle for admin-keyed subcommands.
///
/// Returns enough state to mutate the on-disk config (e.g. write a freshly
/// rotated `api_key` back to the active context).
#[derive(Debug)]
pub struct AdminResolved {
    /// Server base URL (e.g. `https://oyster.example/api/v1`).
    pub url: String,
    /// App admin-key Bearer token.
    pub admin_key: String,
    /// Resolved active context name.
    pub ctx_name: String,
    /// Selected app name within that context.
    pub app_name: String,
    /// Full parsed config — round-tripped on save to preserve unrelated fields.
    pub file: FileConfig,
    /// Path of the config file on disk (always present here).
    pub config_path: PathBuf,
}

/// Resolve the URL + admin key for an admin-scoped subcommand.
///
/// Picks the app automatically when the active context has exactly one;
/// requires `--app <name>` to disambiguate when there are multiple.
pub fn resolve_admin(
    explicit: Option<&Path>,
    cli_context: Option<&str>,
    cli_url: Option<&str>,
    app: Option<&str>,
) -> Result<AdminResolved, ConfigError> {
    let (file, config_path) = load_file_config(explicit)?;
    let config_path = config_path.ok_or(ConfigError::NoConfigFile)?;
    let env_ctx = std::env::var("OYSTER_CONTEXT").ok();
    let ctx_name = require_context_name_with_env(cli_context, env_ctx.as_deref(), &file)?;
    let ctx = file
        .contexts
        .get(&ctx_name)
        .expect("checked by require_context_name_with_env");

    let url = cli_url
        .map(String::from)
        .or_else(|| ctx.url.clone())
        .ok_or(ConfigError::MissingUrl)?;

    let app_name = match app {
        Some(name) => {
            if !ctx.apps.contains_key(name) {
                let known = ctx.apps.keys().cloned().collect::<Vec<_>>().join(", ");
                return Err(ConfigError::UnknownApp {
                    name: name.into(),
                    known,
                });
            }
            name.to_string()
        }
        None => match ctx.apps.len() {
            0 => return Err(ConfigError::MissingApp { ctx: ctx_name }),
            1 => ctx.apps.keys().next().cloned().unwrap(),
            _ => {
                let known = ctx.apps.keys().cloned().collect::<Vec<_>>().join(", ");
                return Err(ConfigError::AmbiguousApp { known });
            }
        },
    };
    let admin_key = ctx.apps[&app_name].admin_key.clone();
    Ok(AdminResolved {
        url,
        admin_key,
        ctx_name,
        app_name,
        file,
        config_path,
    })
}

fn require_context_name_with_env(
    flag: Option<&str>,
    env: Option<&str>,
    file: &FileConfig,
) -> Result<String, ConfigError> {
    resolve_context_name(flag, env, file)?.ok_or(ConfigError::NoActiveContext)
}

/// Serialize `config` and atomically replace `path` with the result.
///
/// The temp file is opened with `O_CREAT | O_EXCL` and (on Unix) mode
/// `0o600` set at file-creation time, so the yaml never exists on disk
/// at a more permissive mode — closing the TOCTOU window between
/// content-write and chmod. `std::fs::rename` over an existing file is
/// atomic on POSIX and on Windows 10+; Oyster targets Linux/macOS
/// developer machines, so this is sufficient.
pub fn save_config(path: &Path, config: &FileConfig) -> Result<(), ConfigError> {
    use std::io::Write;

    let yaml = serde_yaml::to_string(config)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::WriteError {
            path: path.to_owned(),
            source: e,
        })?;
    }
    let tmp = unique_tmp_path(path);

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(|e| ConfigError::WriteError {
        path: tmp.clone(),
        source: e,
    })?;
    if let Err(e) = file
        .write_all(yaml.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConfigError::WriteError {
            path: tmp,
            source: e,
        });
    }
    drop(file);

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConfigError::WriteError {
            path: path.to_owned(),
            source: e,
        });
    }
    Ok(())
}

/// Build a unique temp path next to `path`. Using a unique suffix per
/// call lets us pass `O_EXCL` to `open()` without ever colliding with a
/// stale temp file from a prior crashed run.
fn unique_tmp_path(path: &Path) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_extension(format!("yaml.tmp.{}.{}", std::process::id(), nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> FileConfig {
        let mut testnet = Context {
            url: Some("https://oyster.testnet.example/api/v1".to_string()),
            api_key: Some("testnet-key".to_string()),
            apps: BTreeMap::new(),
        };
        testnet.apps.insert(
            "app-one".to_string(),
            AppEntry {
                admin_key: "admin-key-1".to_string(),
            },
        );
        testnet.apps.insert(
            "app-two".to_string(),
            AppEntry {
                admin_key: "admin-key-2".to_string(),
            },
        );

        let devnet = Context {
            url: Some("https://oyster.devnet.example/api/v1".to_string()),
            api_key: Some("devnet-key".to_string()),
            apps: BTreeMap::new(),
        };

        let mut contexts = BTreeMap::new();
        contexts.insert("testnet".to_string(), testnet);
        contexts.insert("devnet".to_string(), devnet);

        FileConfig {
            active_context: Some("testnet".to_string()),
            contexts,
        }
    }

    #[test]
    fn round_trip_multi_context_yaml() {
        let original = sample_config();
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed: FileConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn context_resolution_flag_beats_env_beats_active() {
        let file = sample_config();

        let picked = resolve_context_name(Some("devnet"), Some("testnet"), &file)
            .unwrap()
            .unwrap();
        assert_eq!(picked, "devnet");

        let picked = resolve_context_name(None, Some("devnet"), &file)
            .unwrap()
            .unwrap();
        assert_eq!(picked, "devnet");

        let picked = resolve_context_name(None, None, &file).unwrap().unwrap();
        assert_eq!(picked, "testnet");
    }

    #[test]
    fn unknown_context_errors_list_known() {
        let file = sample_config();
        let err = resolve_context_name(Some("nope"), None, &file).unwrap_err();
        match err {
            ConfigError::UnknownContext { name, known } => {
                assert_eq!(name, "nope");
                assert_eq!(known, "devnet, testnet");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn single_context_auto_selected_when_no_name_resolved() {
        let mut file = FileConfig::default();
        let ctx = Context {
            url: Some("http://only.example/api/v1".to_string()),
            ..Default::default()
        };
        file.contexts.insert("only".to_string(), ctx);

        let picked = resolve_context_name(None, None, &file).unwrap();
        assert_eq!(picked, Some("only".to_string()));
    }

    #[test]
    fn no_context_when_nothing_resolved_and_multiple_contexts() {
        let mut file = sample_config();
        file.active_context = None;
        let picked = resolve_context_name(None, None, &file).unwrap();
        assert_eq!(picked, None);
    }

    #[test]
    fn old_schema_yaml_fails_to_parse() {
        let old = "url: http://example/api/v1\napi_key: some-key\n";
        let result: Result<FileConfig, _> = serde_yaml::from_str(old);
        assert!(result.is_err(), "old flat schema must not parse anymore");
    }

    #[test]
    fn old_jwt_schema_yaml_fails_to_parse() {
        let old = "active_context: testnet\n\
                   contexts:\n  testnet:\n    apps:\n      legacy:\n        jwt: abc\n        jwt_expiry: 2030-01-01T00:00:00Z\n";
        let result: Result<FileConfig, _> = serde_yaml::from_str(old);
        assert!(result.is_err(), "old jwt schema must not parse anymore");
    }

    #[test]
    fn save_config_round_trip() {
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let original = sample_config();
        save_config(&path, &original).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: FileConfig = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(parsed, original);
    }

    #[cfg(unix)]
    #[test]
    fn save_config_writes_mode_0o600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let path = dir.join("client.yaml");
        save_config(&path, &sample_config()).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn load_file_config_refuses_group_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let yaml = serde_yaml::to_string(&sample_config()).unwrap();
        std::fs::write(&path, yaml).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = load_file_config(Some(&path)).unwrap_err();
        match err {
            ConfigError::InsecurePermissions {
                path: err_path,
                mode,
            } => {
                assert_eq!(err_path, path);
                assert_ne!(mode & 0o077, 0);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_file_config_accepts_mode_0o600() {
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let original = sample_config();
        save_config(&path, &original).unwrap();
        let (loaded, loaded_path) = load_file_config(Some(&path)).unwrap();
        assert_eq!(loaded, original);
        assert_eq!(loaded_path, Some(path));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_admin_picks_single_app() {
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let mut file = FileConfig::default();
        let mut ctx = Context {
            url: Some("https://oyster.example/api/v1".to_string()),
            ..Default::default()
        };
        ctx.apps.insert(
            "only-app".to_string(),
            AppEntry {
                admin_key: "AK1".to_string(),
            },
        );
        file.contexts.insert("only-ctx".to_string(), ctx);
        file.active_context = Some("only-ctx".to_string());
        save_config(&path, &file).unwrap();

        let resolved = resolve_admin(Some(&path), None, None, None).unwrap();
        assert_eq!(resolved.app_name, "only-app");
        assert_eq!(resolved.admin_key, "AK1");
        assert_eq!(resolved.ctx_name, "only-ctx");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_admin_ambiguous_when_multiple_apps() {
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let file = sample_config();
        save_config(&path, &file).unwrap();

        let err = resolve_admin(Some(&path), None, None, None).unwrap_err();
        match err {
            ConfigError::AmbiguousApp { known } => {
                assert!(known.contains("app-one"));
                assert!(known.contains("app-two"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_admin_unknown_app_lists_known() {
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let file = sample_config();
        save_config(&path, &file).unwrap();

        let err = resolve_admin(Some(&path), None, None, Some("nope")).unwrap_err();
        match err {
            ConfigError::UnknownApp { name, known } => {
                assert_eq!(name, "nope");
                assert!(known.contains("app-one"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_admin_missing_app() {
        let dir = tempdir();
        let path = dir.join("client.yaml");
        let mut file = FileConfig::default();
        let ctx = Context {
            url: Some("https://oyster.example/api/v1".to_string()),
            ..Default::default()
        };
        file.contexts.insert("empty".to_string(), ctx);
        file.active_context = Some("empty".to_string());
        save_config(&path, &file).unwrap();

        let err = resolve_admin(Some(&path), None, None, None).unwrap_err();
        match err {
            ConfigError::MissingApp { ctx } => assert_eq!(ctx, "empty"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// Minimal tempdir without pulling in `tempfile` — uses a pid+nanos path
    /// under the system temp dir and cleans up on drop of the caller's scope.
    fn tempdir() -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("oyster-cli-test-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
