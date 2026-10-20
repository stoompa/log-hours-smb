//! Thin wrapper around the 1Password CLI. Nothing secret is written to disk.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

use crate::config::{Config, Provider};

pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
struct Item {
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Field {
    id: String,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

async fn op(args: &[&str]) -> Result<String> {
    let out = Command::new("op")
        .args(args)
        .output()
        .await
        .context("could not run `op` — is the 1Password CLI installed?")?;
    if !out.status.success() {
        bail!(
            "op {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn item_args<'a>(cfg: &'a Config, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["item", "get", cfg.op_item.as_str()];
    if let Some(vault) = &cfg.op_vault {
        args.extend(["--vault", vault.as_str()]);
    }
    args.extend(extra);
    args
}

pub async fn credentials(cfg: &Config) -> Result<Credentials> {
    let json = op(&item_args(cfg, &["--format", "json", "--reveal"])).await?;
    let item: Item = serde_json::from_str(&json).context("unexpected `op item get` output")?;
    let find = |purpose: &str, id: &str| {
        item.fields
            .iter()
            .find(|f| f.purpose.as_deref() == Some(purpose) || f.id == id)
            .and_then(|f| f.value.clone())
            .with_context(|| format!("1Password item `{}` has no {id} field", cfg.op_item))
    };
    Ok(Credentials {
        username: find("USERNAME", "username")?,
        password: find("PASSWORD", "password")?,
    })
}

/// Fetches a fresh one-time password; call right before typing it.
pub async fn otp(cfg: &Config) -> Result<String> {
    op(&item_args(cfg, &["--otp"])).await
}

/// The API key for the configured provider: its env var, else `api_key_ref`
/// from 1Password. A custom OpenAI-compatible `base_url` (a local server) may
/// need no key at all.
pub async fn api_key(cfg: &Config) -> Result<Option<String>> {
    let env = cfg.provider.key_env();
    if let Ok(key) = std::env::var(env)
        && !key.is_empty()
    {
        return Ok(Some(key));
    }
    match cfg.api_key_ref.as_deref() {
        // Checked here so a pasted key is never handed to `op` and echoed in its error.
        Some(reference) if !reference.starts_with("op://") => {
            bail!("`api_key_ref` must be an op:// reference — run setup again")
        }
        Some(reference) => Ok(Some(op(&["read", reference]).await?)),
        None if cfg.provider == Provider::OpenAI && cfg.base_url.is_some() => Ok(None),
        None => bail!("set {env} or `api_key_ref` in the config"),
    }
}
