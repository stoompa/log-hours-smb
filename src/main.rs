mod ai;
mod config;
mod model;
mod secrets;
mod smb;
mod tui;
mod worker;

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate};
use clap::{Parser, Subcommand};

use config::{Config, Provider};

#[derive(Parser)]
#[command(about = "Log SMB HR working hours from a plain-English phrase")]
struct Cli {
    /// Show the browser window instead of running headless.
    #[arg(long, global = true)]
    headed: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Write ~/.config/log-hours-smb/config.toml and check 1Password access.
    Setup,
    /// Log in to SMB and print the entries logged in the given week (read-only).
    Week { date: Option<NaiveDate> },
    /// Show how the AI reads a phrase, without touching SMB.
    Parse { phrase: Vec<String> },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => {
            if !Config::path()?.exists() {
                println!("First run — let's set things up.\n");
                setup().await?;
                println!();
            }
            let cfg = Config::load()?;
            let key = secrets::api_key(&cfg).await?;
            tui::run(cfg, key, cli.headed).await
        }
        Some(Command::Setup) => setup().await,
        Some(Command::Week { date }) => {
            let cfg = Config::load()?;
            let session = smb::Smb::launch(cli.headed).await?;
            let result = async {
                session.login(&cfg, &|line| eprintln!("{line}")).await?;
                let date = date.unwrap_or_else(|| Local::now().date_naive());
                for entry in session.week(date).await? {
                    println!("{}", entry.to_line());
                }
                anyhow::Ok(())
            }
            .await;
            session.close().await;
            result
        }
        Some(Command::Parse { phrase }) => {
            let cfg = Config::load()?;
            let key = secrets::api_key(&cfg).await?;
            let parsed = ai::parse(
                &cfg,
                key.as_deref(),
                &phrase.join(" "),
                Local::now().date_naive(),
            )
            .await?;
            println!("{}", parsed.summary);
            for entry in parsed.entries {
                println!("{}", entry.to_line());
            }
            Ok(())
        }
    }
}

fn ask(prompt: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(d) => print!("  {prompt} [{d}]: "),
        None => print!("  {prompt}: "),
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    // Pasted values often come wrapped in quotes, e.g. a copied 1Password reference.
    let line = line.trim();
    let line = ['"', '\'']
        .iter()
        .find_map(|&q| line.strip_prefix(q)?.strip_suffix(q))
        .unwrap_or(line)
        .trim()
        .to_string();
    Ok(if line.is_empty() {
        default.unwrap_or_default().to_string()
    } else {
        line
    })
}

/// Prints a setup step's heading (blank for a follow-up question) and what it asks for.
fn section(title: &str, lines: &[&str]) {
    println!();
    if !title.is_empty() {
        println!("{title}");
    }
    for line in lines {
        println!("  {line}");
    }
}

async fn setup() -> Result<()> {
    let existing = Config::load().ok();
    println!("Settings go in {}.", Config::path()?.display());
    println!("No passwords or keys are stored there; they are read from 1Password when needed.");
    println!("Press Enter to keep the value shown in [brackets].");

    section(
        "1/4  SMB login",
        &["The company tag is what you type on the first SMB login screen (hr.smb.co.uk)."],
    );
    let company = loop {
        let answer = ask(
            "Company tag",
            existing.as_ref().map(|c| c.company_tag.as_str()),
        )?;
        if !answer.is_empty() {
            break answer;
        }
    };
    section(
        "",
        &[
            "Your SMB username, password and 2FA code are read from a 1Password login item.",
            "It needs a username, a password and a one-time password field.",
        ],
    );
    let item = loop {
        let answer = ask(
            "1Password item name",
            existing.as_ref().map(|c| c.op_item.as_str()),
        )?;
        if !answer.is_empty() {
            break answer;
        }
    };
    let vault = ask(
        "Vault it is in (leave blank to search all vaults)",
        existing.as_ref().and_then(|c| c.op_vault.as_deref()),
    )?;

    section(
        "2/4  Normal working day",
        &[
            "Used when you stage a normal day (n), fill the week (w) or say \"normal day\".",
            "Write each block as start-end, separated by spaces; gaps between blocks are breaks.",
            "Example: 8-1230 1300-1700 means 08:00-12:30, lunch, then 13:00-17:00.",
        ],
    );
    let current_day = existing
        .as_ref()
        .map(|c| c.default_day.join(" "))
        .unwrap_or_else(|| "8-1230 1300-1700".into());
    let default_day = loop {
        let answer = ask("Normal day", Some(&current_day))?;
        match model::parse_day_blocks(&answer) {
            Ok(blocks) => break blocks,
            Err(e) => println!("  {e}"),
        }
    };
    println!("  → {}", default_day.join(", "));
    let mut cfg = Config::new(company, item, (!vault.is_empty()).then_some(vault));
    cfg.default_day = default_day;
    setup_llm(&mut cfg, existing.as_ref())?;
    let path = cfg.save()?;

    section("4/4  Checking", &[]);
    print!("  1Password (approve the prompt if one appears)… ");
    std::io::stdout().flush()?;
    let creds = secrets::credentials(&cfg).await.context(RERUN)?;
    let otp = secrets::otp(&cfg).await.context(RERUN)?;
    println!(
        "ok (user {}, one-time code has {} digits)",
        creds.username,
        otp.len()
    );
    print!("  {}… ", cfg.provider.label());
    std::io::stdout().flush()?;
    let key = secrets::api_key(&cfg).await.context(RERUN)?;
    let parsed = ai::parse(
        &cfg,
        key.as_deref(),
        "normal day today",
        Local::now().date_naive(),
    )
    .await
    .context(RERUN)?;
    println!("ok — \"normal day today\" → {}", parsed.summary);
    println!("\nAll set. Saved {}.", path.display());
    Ok(())
}

const RERUN: &str =
    "setup check failed — settings were saved; fix this and run `log-hours-smb setup` again";

fn setup_llm(cfg: &mut Config, existing: Option<&Config>) -> Result<()> {
    section(
        "3/4  AI for reading plain-English phrases",
        &[
            "Which service turns phrases like \"normal day but left at 3\" into hours:",
            "  claude      Anthropic API, needs an Anthropic API key",
            "  openrouter  openrouter.ai, needs an OpenRouter API key",
            "  openai      OpenAI, or a local server such as Ollama or LM Studio",
        ],
    );
    let current = existing.map(|c| c.provider).unwrap_or_default();
    let provider = loop {
        let answer = ask("Provider", Some(&format!("{current:?}").to_lowercase()))?;
        match Provider::parse(&answer) {
            Some(p) => break p,
            None => println!("  pick claude, openrouter or openai"),
        }
    };
    let same = existing.filter(|c| c.provider == provider);

    if provider == Provider::OpenAI {
        section(
            "",
            &[
                "Where the API lives. Keep the default for OpenAI itself, or use",
                "http://localhost:11434/v1 for Ollama, http://localhost:1234/v1 for LM Studio.",
            ],
        );
        let url = ask(
            "Base URL",
            Some(
                same.map(|c| c.base_url())
                    .unwrap_or(provider.default_base_url()),
            ),
        )?;
        cfg.base_url = (url.trim_end_matches('/') != provider.default_base_url()).then_some(url);
    }

    let model_default = match (same, provider) {
        (Some(c), _) => Some(c.model.clone()),
        (None, Provider::Claude) => Some(config::default_model()),
        (None, _) => None,
    };
    section(
        "",
        &[match provider {
            Provider::Claude => {
                "Model: haiku is fast, cheap and enough for this; try sonnet if phrases are misread."
            }
            Provider::OpenRouter => "Model: its id as listed on openrouter.ai/models.",
            Provider::OpenAI => "Model: its name (for Ollama, one from `ollama list`).",
        }],
    );
    cfg.model = loop {
        let model = ask("Model", model_default.as_deref())?;
        if !model.is_empty() {
            break match provider {
                Provider::Claude => config::claude_model(&model),
                _ => model,
            };
        }
    };

    let local = provider == Provider::OpenAI && cfg.base_url.is_some();
    let env = provider.key_env();
    let env_set = std::env::var(env).is_ok_and(|k| !k.is_empty());
    let blank = if env_set {
        format!("{env} is set in this shell, so you can leave this blank to use it.")
    } else if local {
        "A local server usually needs no key; leave this blank.".to_string()
    } else {
        format!("Or leave it blank and set {env} in your shell profile instead.")
    };
    section(
        "",
        &[
            "API key: don't paste the key itself here. Save it in 1Password, right-click",
            "the field, choose Copy Secret Reference and paste that (it starts with op://).",
            &blank,
        ],
    );
    let key_ref = loop {
        let key_ref = ask(
            "Secret reference",
            same.and_then(|c| c.api_key_ref.as_deref()),
        )?;
        if !key_ref.is_empty() && !key_ref.starts_with("op://") {
            println!(
                "  that isn't an op:// reference, so it wasn't saved; paste the Secret Reference instead"
            );
            continue;
        }
        if !key_ref.is_empty() || env_set || local {
            break key_ref;
        }
        println!("  {env} isn't set in this shell, so a secret reference is needed");
    };
    cfg.provider = provider;
    cfg.api_key_ref = (!key_ref.is_empty()).then_some(key_ref);
    Ok(())
}
