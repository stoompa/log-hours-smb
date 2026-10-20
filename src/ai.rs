//! Turns a phrase like "9 to 17:30 today, 30 min lunch" into entries, using
//! Claude with a JSON-schema constrained response.

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::model::Entry;

const API_URL: &str = "https://api.anthropic.com/v1/messages";

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

fn system_prompt(cfg: &Config, today: NaiveDate) -> String {
    format!(
        "You convert a person's description of their working hours into timesheet entries.\n\
         Today is {weekday} {today}. Weeks start on Monday.\n\
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

pub fn request_body(cfg: &Config, phrase: &str, today: NaiveDate) -> Value {
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

pub fn parse_response(body: &Value) -> Result<Parsed> {
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
    let parsed: Parsed =
        serde_json::from_str(&text).with_context(|| format!("unexpected response: {text}"))?;
    for entry in &parsed.entries {
        entry.validate()?;
    }
    Ok(parsed)
}

pub async fn parse(cfg: &Config, api_key: &str, phrase: &str, today: NaiveDate) -> Result<Parsed> {
    let resp = reqwest::Client::new()
        .post(API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(&request_body(cfg, phrase, today))
        .send()
        .await
        .context("could not reach the Claude API")?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        bail!(
            "Claude API {status}: {}",
            body["error"]["message"].as_str().unwrap_or("unknown error")
        );
    }
    parse_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::new("acme".into(), "SMB".into(), None, None)
    }

    #[test]
    fn body_includes_date_and_schema() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let body = request_body(&cfg(), "normal day today", today);
        assert!(
            body["system"]
                .as_str()
                .unwrap()
                .contains("Tuesday 2026-10-20")
        );
        assert!(body["system"].as_str().unwrap().contains("12:30-13:00"));
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    }

    #[test]
    fn parses_structured_text_block() {
        let body = json!({
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "{\"entries\":[{\"date\":\"2026-10-20\",\"start\":\"09:00\",\"end\":\"12:30\"},{\"date\":\"2026-10-20\",\"start\":\"13:00\",\"end\":\"17:30\"}],\"summary\":\"Today 9-17:30, lunch 12:30-13:00\"}"}
            ]
        });
        let parsed = parse_response(&body).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[1].to_line(), "2026-10-20 13:00-17:30");
    }

    #[test]
    fn surfaces_refusals() {
        let body = json!({"stop_reason": "refusal", "content": []});
        assert!(parse_response(&body).is_err());
    }

    #[test]
    fn rejects_invalid_blocks() {
        let body = json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{\"entries\":[{\"date\":\"2026-10-20\",\"start\":\"17:00\",\"end\":\"09:00\"}],\"summary\":\"\"}"}]
        });
        assert!(parse_response(&body).is_err());
    }
}
