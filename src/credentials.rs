//! Credential-store access with explicit environment-variable support.
use crate::config::ModelConfig;
use anyhow::{Context, Result, bail};

pub struct Secret(String);
impl Secret {
    pub fn new(value: String) -> Result<Self> {
        if value.trim().is_empty() {
            bail!("API key cannot be empty");
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            text.into()
        } else {
            text.replace(&self.0, "[REDACTED]")
        }
    }
}

pub fn resolve(alias: &str, model: &ModelConfig) -> Result<Secret> {
    if let Some(name) = &model.key_env {
        let value = std::env::var(name)
            .with_context(|| format!("set the configured environment variable {name}"))?;
        if value.trim().is_empty() {
            bail!("configured API key is empty");
        }
        return Ok(Secret(value));
    }
    let value = keyring::Entry::new("forge-harness", alias)?
        .get_password()
        .context("no API key in credential store; use forge auth add or configure key_env")?;
    if value.trim().is_empty() {
        bail!("stored API key is empty");
    }
    Ok(Secret(value))
}

pub fn add(alias: &str, key: &str) -> Result<()> {
    if key.trim().is_empty() {
        bail!("API key cannot be empty");
    }
    keyring::Entry::new("forge-harness", alias)?
        .set_password(key)
        .context("OS credential service unavailable; use key_env instead")
}
pub fn remove(alias: &str) -> Result<()> {
    keyring::Entry::new("forge-harness", alias)?
        .delete_credential()
        .context("removing stored API key")
}
