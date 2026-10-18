use anyhow::{Context, Result, ensure};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub folder: PathBuf,
    pub api_key: String,
    /// Paperless base URL, including any installation subpath; not an /api/ URL.
    pub endpoint_url: String,
    pub tag_ids: Vec<u64>,
    pub recursive: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            folder: PathBuf::new(),
            api_key: String::new(),
            endpoint_url: "https://paperless.example.com".into(),
            tag_ids: vec![],
            recursive: false,
        }
    }
}
pub fn resolve(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match path {
        Some(path) => path,
        None => ProjectDirs::from("", "", "ngxd")
            .context("Cannot determine user config directory; supply --config")?
            .config_dir()
            .join("config.json"),
    };
    Ok(std::path::absolute(path)?)
}
pub fn load_or_create(path: &Path) -> Result<Config> {
    fs::create_dir_all(path.parent().context("Config needs a parent directory")?)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(serde_json::to_string_pretty(&Config::default())?.as_bytes())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("Creating config"),
    }
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("Invalid config: {}", path.display()))
}
impl Config {
    pub fn validate(&self, config_path: &Path) -> Result<()> {
        ensure!(
            self.folder.is_absolute() && self.folder.is_dir(),
            "folder must be an existing absolute directory"
        );
        ensure!(!self.api_key.trim().is_empty(), "api_key is required");
        ensure!(
            self.tag_ids.iter().all(|id| *id > 0),
            "tag_ids must be positive integers"
        );
        let url = self.base_url()?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "endpoint_url must be HTTP(S)"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "endpoint_url must not contain credentials, query, or fragment"
        );
        ensure!(
            !url.path().trim_end_matches('/').ends_with("/api"),
            "endpoint_url must be the Paperless base URL, without /api"
        );
        let folder = fs::canonicalize(&self.folder)?;
        ensure!(
            !fs::canonicalize(config_path)?.starts_with(&folder),
            "Keep config and queue outside the watched folder"
        );
        ensure!(
            !fs::canonicalize(config_path.parent().unwrap())?.starts_with(&folder),
            "Keep config and queue outside the watched folder"
        );
        Ok(())
    }
    pub fn base_url(&self) -> Result<reqwest::Url> {
        reqwest::Url::parse(&format!("{}/", self.endpoint_url.trim_end_matches('/')))
            .context("Invalid endpoint_url")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_round_trip_and_optional_tags() {
        let data = serde_json::to_vec(&Config::default()).unwrap();
        let config: Config = serde_json::from_slice(&data).unwrap();
        assert!(config.tag_ids.is_empty());
        assert!(!config.recursive);
        assert!(!serde_json::from_str::<Config>("{}").unwrap().recursive);
        assert!(
            serde_json::from_str::<Config>(r#"{"recursive":true}"#)
                .unwrap()
                .recursive
        );
        assert!(serde_json::from_str::<Config>(r#"{"tag_is": [1]}"#).is_err());
        assert!(serde_json::from_str::<Config>(r#"{"tag_ids": [-1]}"#).is_err());
    }
}
