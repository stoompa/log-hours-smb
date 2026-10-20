use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

use crate::model::parse_time;

/// Which API turns phrases into entries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Anthropic Messages API.
    #[default]
    Claude,
    /// OpenRouter's OpenAI-compatible API.
    OpenRouter,
    /// OpenAI, or any OpenAI-compatible server via `base_url` (Ollama, LM Studio, vLLM…).
    OpenAI,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::OpenRouter => "OpenRouter",
            Provider::OpenAI => "OpenAI-compatible API",
        }
    }

    pub fn key_env(self) -> &'static str {
        match self {
            Provider::Claude => "ANTHROPIC_API_KEY",
            Provider::OpenRouter => "OPENROUTER_API_KEY",
            Provider::OpenAI => "OPENAI_API_KEY",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Provider::Claude => "https://api.anthropic.com/v1",
            Provider::OpenRouter => "https://openrouter.ai/api/v1",
            Provider::OpenAI => "https://api.openai.com/v1",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "claude" | "anthropic" => Some(Provider::Claude),
            "openrouter" => Some(Provider::OpenRouter),
            "openai" | "local" | "ollama" => Some(Provider::OpenAI),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The "Company Tag" asked for on the first login screen.
    pub company_tag: String,
    /// 1Password item holding the SMB username, password and one-time password.
    pub op_item: String,
    pub op_vault: Option<String>,
    #[serde(default)]
    pub provider: Provider,
    #[serde(default = "default_model")]
    pub model: String,
    /// Overrides the provider's endpoint, e.g. `http://localhost:11434/v1` for Ollama.
    pub base_url: Option<String>,
    /// `op://…` reference for the provider's API key; the provider's env var wins if set.
    #[serde(alias = "anthropic_key_ref")]
    pub api_key_ref: Option<String>,
    /// A normal working day, as `HH:MM-HH:MM` blocks. Used when a phrase says
    /// "normal day" or gives no times.
    #[serde(default = "default_day")]
    pub default_day: Vec<String>,
}

pub fn default_model() -> String {
    "claude-opus-5-5".into()
}

/// Expands a bare family name ("haiku") to a full Claude model id.
pub fn claude_model(name: &str) -> String {
    match name.trim().to_lowercase().as_str() {
        "opus" => "claude-opus-5-5".into(),
        "sonnet" => "claude-sonnet-5-5".into(),
        "haiku" => "claude-haiku-5-5".into(),
        "fable" => "claude-fable-5-1".into(),
        _ => name.trim().to_string(),
    }
}

fn default_day() -> Vec<String> {
    vec!["08:00-12:30".into(), "13:00-17:00".into()]
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("LOG_HOURS_SMB_CONFIG") {
            return Ok(PathBuf::from(path));
        }
        let config = dirs::home_dir().context("no home directory")?.join(".config");
        let dir = config.join("log-hours-smb");
        // Carry over the config from before the rename from automate-smb.
        let old = config.join("automate-smb");
        if !dir.exists() && old.exists() {
            std::fs::rename(&old, &dir)?;
        }
        Ok(dir.join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no config at {} — run `log-hours-smb setup` first",
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

    /// The configured normal day as (start, end) pairs; malformed blocks are skipped.
    pub fn default_blocks(&self) -> Vec<(NaiveTime, NaiveTime)> {
        self.default_day
            .iter()
            .filter_map(|b| {
                let (s, e) = b.split_once('-')?;
                Some((parse_time(s).ok()?, parse_time(e).ok()?))
            })
            .collect()
    }

    pub fn base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or(self.provider.default_base_url())
    }

    pub fn new(company_tag: String, op_item: String, op_vault: Option<String>) -> Self {
        Config {
            company_tag,
            op_item,
            op_vault,
            provider: Provider::Claude,
            model: default_model(),
            base_url: None,
            api_key_ref: None,
            default_day: default_day(),
        }
    }
}
