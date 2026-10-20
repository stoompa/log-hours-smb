mod ai;
mod config;
mod model;
mod secrets;
mod smb;
mod tui;
mod worker;

use std::io::{BufRead, Write};

use anyhow::Result;
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
        Some(d) => print!("{prompt} [{d}]: "),
        None => print!("{prompt}: "),
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim().to_string();
    Ok(if line.is_empty() {
        default.unwrap_or_default().to_string()
    } else {
        line
    })
}

async fn setup() -> Result<()> {
    let existing = Config::load().ok();
    let company = ask(
        "SMB company tag",
        existing.as_ref().map(|c| c.company_tag.as_str()),
    )?;
    let item = ask(
        "1Password item for SMB",
        existing.as_ref().map(|c| c.op_item.as_str()),
    )?;
    let vault = ask(
        "1Password vault (blank = search all)",
        existing.as_ref().and_then(|c| c.op_vault.as_deref()),
    )?;
    let current_day = existing
        .as_ref()
        .map(|c| c.default_day.join(" "))
        .unwrap_or_else(|| "8-1230 1300-1700".into());
    let default_day = loop {
        let answer = ask(
            "Normal working day, as blocks with breaks between them (e.g. 8-1230 1300-1700)",
            Some(&current_day),
        )?;
        match model::parse_day_blocks(&answer) {
            Ok(blocks) => break blocks,
            Err(e) => println!("  {e}"),
        }
    };
    println!("  normal day: {}", default_day.join(", "));
    let mut cfg = Config::new(company, item, (!vault.is_empty()).then_some(vault));
    cfg.default_day = default_day;
    setup_llm(&mut cfg, existing.as_ref())?;
    let path = cfg.save()?;
    println!("saved {}", path.display());

    print!("checking 1Password… ");
    std::io::stdout().flush()?;
    let creds = secrets::credentials(&cfg).await?;
    let otp = secrets::otp(&cfg).await?;
    println!(
        "ok (user {}, one-time code has {} digits)",
        creds.username,
        otp.len()
    );
    print!("checking {}… ", cfg.provider.label());
    std::io::stdout().flush()?;
    let key = secrets::api_key(&cfg).await?;
    let parsed = ai::parse(
        &cfg,
        key.as_deref(),
        "normal day today",
        Local::now().date_naive(),
    )
    .await?;
    println!("ok — \"normal day today\" → {}", parsed.summary);
    Ok(())
}

fn setup_llm(cfg: &mut Config, existing: Option<&Config>) -> Result<()> {
    let current = existing.map(|c| c.provider).unwrap_or_default();
    let provider = loop {
        let answer = ask(
            "AI provider: claude, openrouter, or openai (any OpenAI-compatible server, e.g. Ollama)",
            Some(&format!("{current:?}").to_lowercase()),
        )?;
        match Provider::parse(&answer) {
            Some(p) => break p,
            None => println!("  pick claude, openrouter or openai"),
        }
    };
    let same = existing.filter(|c| c.provider == provider);

    if provider == Provider::OpenAI {
        let url = ask(
            "Base URL (Ollama: http://localhost:11434/v1, LM Studio: http://localhost:1234/v1)",
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
    let hint = match provider {
        Provider::Claude => "Model",
        Provider::OpenRouter => "Model id as listed on openrouter.ai/models",
        Provider::OpenAI => "Model name (for Ollama, one from `ollama list`)",
    };
    cfg.model = loop {
        let model = ask(hint, model_default.as_deref())?;
        if !model.is_empty() {
            break model;
        }
    };

    let local = provider == Provider::OpenAI && cfg.base_url.is_some();
    let key_ref = ask(
        &format!(
            "op:// reference for the API key (blank = use {}{})",
            provider.key_env(),
            if local {
                ", or none for a local server"
            } else {
                ""
            }
        ),
        same.and_then(|c| c.api_key_ref.as_deref()),
    )?;
    cfg.provider = provider;
    cfg.api_key_ref = (!key_ref.is_empty()).then_some(key_ref);
    Ok(())
}
