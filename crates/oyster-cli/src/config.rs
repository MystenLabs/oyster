use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing url: set --url or add 'url' to config file")]
    MissingUrl,
    #[error("missing api_key: set --api-key or add 'api_key' to config file")]
    MissingApiKey,
    #[error("failed to read config file {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    ParseError {
        path: PathBuf,
        source: serde_yaml::Error,
    },
}

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub url: Option<String>,
    pub api_key: Option<String>,
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

/// Resolve config for authenticated commands (requires both url and api_key).
pub fn resolve(
    explicit: Option<&Path>,
    cli_url: Option<&str>,
    cli_api_key: Option<&str>,
) -> Result<ResolvedConfig, ConfigError> {
    let (file, _config_path) = load_file_config(explicit)?;

    let url = cli_url
        .map(String::from)
        .or(file.url)
        .ok_or(ConfigError::MissingUrl)?;

    let api_key = cli_api_key
        .map(String::from)
        .or(file.api_key)
        .ok_or(ConfigError::MissingApiKey)?;

    Ok(ResolvedConfig { url, api_key })
}

/// Resolve config for public commands (only requires url).
pub fn resolve_url_only(
    explicit: Option<&Path>,
    cli_url: Option<&str>,
) -> Result<(String, Option<PathBuf>), ConfigError> {
    let (file, config_path) = load_file_config(explicit)?;

    let url = cli_url
        .map(String::from)
        .or(file.url)
        .ok_or(ConfigError::MissingUrl)?;

    Ok((url, config_path))
}
