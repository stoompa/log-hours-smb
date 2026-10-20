//! Turns a phrase like "9 to 17:30 today, 30 min lunch" into entries. Claude
//! goes through the Messages API; OpenRouter, OpenAI and any other
//! OpenAI-compatible server (Ollama, LM Studio, vLLM…) go through
//! `/chat/completions`. Both are constrained to the same JSON schema.

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Config, Provider};
use crate::model::{Entry, week_start};

const CLAUDE_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Deserialize)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// One line explaining the interpretation, or a question if the phrase was ambiguous.
    pub summary: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "entries": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "date": {"type": "string", "description": "YYYY-MM-DD"},
                        "start": {"type": "string", "description": "HH:MM, 24h"},
                        "end": {"type": "string", "description": "HH:MM, 24h"}
                    },
                    "required": ["date", "start", "end"],
                    "additionalProperties": false
                }
            },
            "summary": {"type": "string"}
        },
        "required": ["entries", "summary"],
        "additionalProperties": false
    })
}

/// Last, this and next week spelled out, so models look dates up instead of
/// doing calendar arithmetic.
fn calendar(today: NaiveDate) -> String {
    let monday = week_start(today);
    [("last week", -7), ("this week", 0), ("next week", 7)]
        .iter()
        .map(|(name, offset)| {
            let days = (0..7)
                .map(|d| monday + chrono::Duration::days(offset + d))
                .map(|d| format!("{} {d}", d.format("%a")))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}: {days}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn system_prompt(cfg: &Config, today: NaiveDate) -> String {
    format!(
        "You convert a person's description of their working hours into timesheet entries.\n\
         Today is {weekday} {today} (week {week}). Weeks start on Monday. Dates:\n{calendar}\n\
         Each entry is one continuous block of work on one date. A break splits the day into \
         two blocks: \"9 to 17:30 with 30 min lunch\" becomes two blocks around a lunch gap. \
         Place an unspecified lunch break at {lunch} unless the phrase says otherwise.\n\
         A normal working day is: {day}. Use it for \"normal day\", \"usual hours\", or when \
         a day is mentioned without times.\n\
         Days described as off, holiday, or sick get no entries. Only include Saturday or \
         Sunday when the phrase names them explicitly.\n\
         In `summary`, state in one short line how you read the phrase. If something is \
         genuinely ambiguous, return no entries and ask the question in `summary`.",
        weekday = today.format("%A"),
        week = today.iso_week().week(),
        calendar = calendar(today),
        day = cfg.default_day.join(" and "),
        lunch = lunch_hint(cfg),
    )
}

fn lunch_hint(cfg: &Config) -> String {
    match cfg.default_day.as_slice() {
        [first, second, ..] => {
            let end = first.split('-').nth(1).unwrap_or("12:30");
            let start = second.split('-').next().unwrap_or("13:00");
            format!("{end}-{start}")
        }
        _ => "12:30-13:00".into(),
    }
}

/// Parses the model's JSON reply, tolerating a Markdown code fence around it.
fn parse_json(text: &str) -> Result<Parsed> {
    let trimmed = text.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed);
    let parsed: Parsed =
        serde_json::from_str(json).with_context(|| format!("unexpected response: {text}"))?;
    for entry in &parsed.entries {
        entry.validate()?;
    }
    Ok(parsed)
}

pub async fn parse(
    cfg: &Config,
    api_key: Option<&str>,
    phrase: &str,
    today: NaiveDate,
) -> Result<Parsed> {
    match cfg.provider {
        Provider::Claude => {
            claude(cfg, api_key.context("no Anthropic API key")?, phrase, today).await
        }
        Provider::OpenRouter | Provider::OpenAI => openai(cfg, api_key, phrase, today).await,
    }
}

// ---- Claude (Messages API) ----------------------------------------------

pub fn claude_body(cfg: &Config, phrase: &str, today: NaiveDate) -> Value {
    json!({
        "model": cfg.model,
        "max_tokens": 4096,
        "fallbacks": "default",
        "system": system_prompt(cfg, today),
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": schema()}
        },
        "messages": [{"role": "user", "content": phrase}]
    })
}

pub fn claude_response(body: &Value) -> Result<Parsed> {
    match body["stop_reason"].as_str() {
        Some("refusal") => bail!("Claude declined to parse that phrase"),
        Some("max_tokens") => bail!("response was cut off; try a shorter phrase"),
        _ => {}
    }
    let text = body["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<String>();
    parse_json(&text)
}

async fn claude(cfg: &Config, api_key: &str, phrase: &str, today: NaiveDate) -> Result<Parsed> {
    let resp = reqwest::Client::new()
        .post(CLAUDE_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(&claude_body(cfg, phrase, today))
        .send()
        .await
        .context("could not reach the Claude API")?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        bail!("Claude API {status}: {}", error_message(&body));
    }
    claude_response(&body)
}

// ---- OpenAI-compatible (/chat/completions) ------------------------------

/// `strict` asks for a schema-constrained reply. Servers that don't support
/// `response_format` get the schema in the prompt instead.
pub fn openai_body(cfg: &Config, phrase: &str, today: NaiveDate, strict: bool) -> Value {
    let mut system = system_prompt(cfg, today);
    if !strict {
        system.push_str(&format!(
            "\nReply with only a JSON object matching this JSON schema, no other text:\n{}",
            schema()
        ));
    }
    let mut body = json!({
        "model": cfg.model,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": phrase}
        ]
    });
    if strict {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {"name": "timesheet_entries", "strict": true, "schema": schema()}
        });
    }
    body
}

pub fn openai_response(body: &Value) -> Result<Parsed> {
    let choice = &body["choices"][0];
    if let Some(refusal) = choice["message"]["refusal"].as_str() {
        bail!("the model declined: {refusal}");
    }
    if choice["finish_reason"] == "length" {
        bail!("response was cut off; try a shorter phrase");
    }
    let text = choice["message"]["content"]
        .as_str()
        .with_context(|| format!("unexpected response: {body}"))?;
    parse_json(text)
}

async fn openai(
    cfg: &Config,
    api_key: Option<&str>,
    phrase: &str,
    today: NaiveDate,
) -> Result<Parsed> {
    let url = format!("{}/chat/completions", cfg.base_url().trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut last_error = String::new();
    for strict in [true, false] {
        let mut req = client
            .post(&url)
            .json(&openai_body(cfg, phrase, today, strict));
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        if cfg.provider == Provider::OpenRouter {
            req = req.header("X-Title", "automate-smb");
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("could not reach {url}"))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return openai_response(&body);
        }
        last_error = format!(
            "{} {status}: {}",
            cfg.provider.label(),
            error_message(&body)
        );
        // Only a rejected request is worth retrying without `response_format`.
        if status != reqwest::StatusCode::BAD_REQUEST
            && status != reqwest::StatusCode::UNPROCESSABLE_ENTITY
        {
            break;
        }
    }
    bail!(last_error)
}

fn error_message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or("unknown error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::new("acme".into(), "SMB".into(), None)
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()
    }

    #[test]
    fn prompt_includes_date_table_and_lunch() {
        let system = system_prompt(&cfg(), today());
        assert!(system.contains("Today is Tuesday 2026-10-20"));
        assert!(system.contains("last week: Mon 2026-10-12"));
        assert!(system.contains("next week: Mon 2026-10-26"));
        assert!(system.contains("12:30-13:00"));
    }

    #[test]
    fn claude_body_uses_structured_output() {
        let body = claude_body(&cfg(), "normal day today", today());
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["model"], "claude-opus-5-5");
    }

    #[test]
    fn openai_body_strict_and_fallback() {
        let strict = openai_body(&cfg(), "x", today(), true);
        assert_eq!(strict["response_format"]["type"], "json_schema");
        let loose = openai_body(&cfg(), "x", today(), false);
        assert!(loose.get("response_format").is_none());
        assert!(
            loose["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("JSON schema")
        );
    }

    #[test]
    fn parses_claude_text_block() {
        let body = json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"entries\":[{\"date\":\"2026-10-20\",\"start\":\"09:00\",\"end\":\"12:30\"},{\"date\":\"2026-10-20\",\"start\":\"13:00\",\"end\":\"17:30\"}],\"summary\":\"Today 9-17:30\"}"}
            ]
        });
        let parsed = claude_response(&body).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[1].to_line(), "2026-10-20 13:00-17:30");
    }

    #[test]
    fn parses_openai_choice_with_code_fence() {
        let body = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "```json\n{\"entries\":[{\"date\":\"2026-10-20\",\"start\":\"13:00\",\"end\":\"17:00\"}],\"summary\":\"ok\"}\n```"}
            }]
        });
        let parsed = openai_response(&body).unwrap();
        assert_eq!(parsed.entries[0].to_line(), "2026-10-20 13:00-17:00");
    }

    #[test]
    fn surfaces_refusals_and_bad_blocks() {
        assert!(claude_response(&json!({"stop_reason": "refusal", "content": []})).is_err());
        let refused = json!({"choices": [{"message": {"refusal": "no", "content": null}}]});
        assert!(openai_response(&refused).is_err());
        let reversed = json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{\"entries\":[{\"date\":\"2026-10-20\",\"start\":\"17:00\",\"end\":\"09:00\"}],\"summary\":\"\"}"}]
        });
        assert!(claude_response(&reversed).is_err());
    }
}
