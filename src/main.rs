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

use config::Config;

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
    /// Write ~/.config/automate-smb/config.toml and check 1Password access.
    Setup,
    /// Log in to SMB and print the entries logged in the given week (read-only).
    Week { date: Option<NaiveDate> },
    /// Show how Claude reads a phrase, without touching SMB.
    Parse { phrase: Vec<String> },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => {
            let cfg = Config::load()?;
            let key = secrets::anthropic_key(&cfg).await?;
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
            let key = secrets::anthropic_key(&cfg).await?;
            let parsed =
                ai::parse(&cfg, &key, &phrase.join(" "), Local::now().date_naive()).await?;
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
    let key_ref = ask(
        "op:// reference for the Anthropic API key (blank = use ANTHROPIC_API_KEY)",
        existing
            .as_ref()
            .and_then(|c| c.anthropic_key_ref.as_deref()),
    )?;
    let mut cfg = Config::new(
        company,
        item,
        (!vault.is_empty()).then_some(vault),
        (!key_ref.is_empty()).then_some(key_ref),
    );
    if let Some(old) = existing {
        cfg.model = old.model;
        cfg.default_day = old.default_day;
    }
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
    match secrets::anthropic_key(&cfg).await {
        Ok(_) => println!("Anthropic API key: ok"),
        Err(e) => println!("Anthropic API key: {e:#}"),
    }
    Ok(())
}
