//! User-owned provider configuration; project files only affect workspace preferences.
use crate::accounting::Pricing;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    Anthropic,
    Compatible,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: ProviderKind,
    pub model: String,
    pub base_url: String,
    pub key_env: Option<String>,
    pub context_tokens: u64,
    pub max_output_tokens: u64,
    pub pricing: Option<Pricing>,
}

impl ModelConfig {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.base_url).context("invalid provider base URL")?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            bail!("provider URL must use HTTPS (HTTP is allowed only on loopback)");
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("provider URL cannot contain credentials or a query string");
        }
        if self.model.is_empty()
            || self.max_output_tokens == 0
            || self.context_tokens <= self.max_output_tokens
        {
            bail!("model needs a name and context limit larger than its positive output limit");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelConfig>,
    pub shell: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub instructions: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub database: PathBuf,
}

impl Paths {
    pub fn discover(home: Option<&Path>) -> Result<Self> {
        if let Some(home) = home {
            return Ok(Self {
                config: home.join("config.toml"),
                database: home.join("forge.db"),
            });
        }
        let dirs = directories::ProjectDirs::from("dev", "forge", "forge")
            .context("could not locate user configuration directory; pass --home")?;
        Ok(Self {
            config: dirs.config_dir().join("config.toml"),
            database: dirs.data_local_dir().join("forge.db"),
        })
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).context("invalid user configuration"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("reading user configuration"),
        }
    }
    pub fn model(&self, alias: Option<&str>) -> Result<(&str, &ModelConfig)> {
        let alias = alias
            .or(self.default_model.as_deref())
            .context("no model selected; configure config.toml (see forge doctor)")?;
        let (name, model) = self
            .models
            .get_key_value(alias)
            .context("unknown model alias")?;
        model.validate()?;
        Ok((name, model))
    }
}

pub fn project(root: &Path) -> Result<ProjectConfig> {
    let path = root.join(".forge.toml");
    if !path.exists() {
        return Ok(ProjectConfig::default());
    }
    let target = path.canonicalize()?;
    if !target.starts_with(root) {
        bail!("project configuration escapes workspace");
    }
    toml::from_str(&std::fs::read_to_string(target)?)
        .context("invalid .forge.toml; only instructions is supported")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_cannot_override_provider_or_permissions() {
        assert!(toml::from_str::<ProjectConfig>("shell='sh'").is_err());
        assert!(toml::from_str::<ProjectConfig>("base_url='https://attacker.invalid'").is_err());
        assert!(toml::from_str::<ProjectConfig>("auto_approve=true").is_err());
    }
    #[test]
    fn endpoints_cannot_smuggle_credentials() {
        let mut m = ModelConfig {
            provider: ProviderKind::Compatible,
            model: "test".into(),
            base_url: "http://remote.invalid/v1".into(),
            key_env: None,
            context_tokens: 10000,
            max_output_tokens: 100,
            pricing: None,
        };
        assert!(m.validate().is_err());
        m.base_url = "https://secret@remote.invalid/v1".into();
        assert!(m.validate().is_err());
        m.base_url = "https://remote.invalid/v1?key=secret".into();
        assert!(m.validate().is_err());
        m.base_url = "http://127.0.0.1:1234/v1".into();
        assert!(m.validate().is_ok());
    }
}
