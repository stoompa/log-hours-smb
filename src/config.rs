use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The "Company Tag" asked for on the first login screen.
    pub company_tag: String,
    /// 1Password item holding the SMB username, password and one-time password.
    pub op_item: String,
    pub op_vault: Option<String>,
    /// `op://…` reference for the Anthropic API key; `ANTHROPIC_API_KEY` wins if set.
    pub anthropic_key_ref: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    /// A normal working day, as `HH:MM-HH:MM` blocks. Used when a phrase says
    /// "normal day" or gives no times.
    #[serde(default = "default_day")]
    pub default_day: Vec<String>,
}

fn default_model() -> String {
    "claude-opus-5-5".into()
}

fn default_day() -> Vec<String> {
    vec!["08:00-12:30".into(), "13:00-17:00".into()]
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("no home directory")?;
        Ok(home.join(".config/automate-smb/config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no config at {} — run `automate-smb setup` first",
                path.display()
            )
        })?;
        toml::from_str(&text).with_context(|| format!("invalid config at {}", path.display()))
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(path)
    }

    pub fn new(
        company_tag: String,
        op_item: String,
        op_vault: Option<String>,
        anthropic_key_ref: Option<String>,
    ) -> Self {
        Config {
            company_tag,
            op_item,
            op_vault,
            anthropic_key_ref,
            model: default_model(),
            default_day: default_day(),
        }
    }
}
