use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use serde::{Deserialize, Serialize};

/// One working-time block as SMB stores it: a date plus start/end. A day with
/// a lunch break is two blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub date: NaiveDate,
    #[serde(with = "hhmm")]
    pub start: NaiveTime,
    #[serde(with = "hhmm")]
    pub end: NaiveTime,
}

impl Entry {
    pub fn validate(&self) -> Result<()> {
        if self.start >= self.end {
            bail!(
                "{}: start {} is not before end {}",
                self.date,
                fmt_time(self.start),
                fmt_time(self.end)
            );
        }
        if (self.end - self.start).num_hours() > 12 {
            bail!("{}: block longer than 12h", self.date);
        }
        Ok(())
    }

    pub fn overlaps(&self, other: &Entry) -> bool {
        self.date == other.date && self.start < other.end && other.start < self.end
    }

    /// Parses `YYYY-MM-DD HH:MM-HH:MM`.
    #[cfg(test)]
    pub fn parse_line(line: &str) -> Result<Self> {
        let (date, times) = line
            .trim()
            .split_once(' ')
            .context("expected `YYYY-MM-DD HH:MM-HH:MM`")?;
        let (start, end) = times
            .trim()
            .split_once('-')
            .context("expected `HH:MM-HH:MM`")?;
        let entry = Entry {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d")?,
            start: parse_time(start)?,
            end: parse_time(end)?,
        };
        entry.validate()?;
        Ok(entry)
    }

    pub fn to_line(&self) -> String {
        format!(
            "{} {}-{}",
            self.date,
            fmt_time(self.start),
            fmt_time(self.end)
        )
    }
}

pub fn week_start(date: NaiveDate) -> NaiveDate {
    date - chrono::Duration::days(date.weekday().num_days_from_monday() as i64)
}

/// Minutes-of-day helpers for the time picker.
pub fn shift_time(t: NaiveTime, minutes: i64) -> NaiveTime {
    let total = (t.hour() as i64 * 60 + t.minute() as i64 + minutes).clamp(0, 23 * 60 + 45);
    NaiveTime::from_hms_opt((total / 60) as u32, (total % 60) as u32, 0).unwrap()
}

pub fn parse_time(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").with_context(|| format!("bad time `{s}`"))
}

/// Accepts the ways people type times: `8`, `830`, `0830`, `8:30`, `13.00`.
pub fn parse_loose_time(s: &str) -> Result<NaiveTime> {
    let digits: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | '.' | 'h'))
        .collect();
    if digits.is_empty() || digits.len() > 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        bail!("bad time `{s}`");
    }
    let (h, m) = if digits.len() <= 2 {
        (digits.as_str(), "0")
    } else {
        digits.split_at(digits.len() - 2)
    };
    NaiveTime::from_hms_opt(h.parse()?, m.parse()?, 0).with_context(|| format!("bad time `{s}`"))
}

/// Parses a normal working day such as `8-1230 1300-1700` or
/// `08:00-12:30, 13:00-17:00` into ordered, non-overlapping `HH:MM-HH:MM` blocks.
pub fn parse_day_blocks(input: &str) -> Result<Vec<String>> {
    let mut text = input
        .replace(['–', '—'], "-")
        .replace([',', ';'], " ")
        .replace(" and ", " ");
    while text.contains(" -") || text.contains("- ") {
        text = text.replace(" -", "-").replace("- ", "-");
    }
    let mut blocks: Vec<(NaiveTime, NaiveTime)> = Vec::new();
    for token in text.split_whitespace() {
        let (start, end) = token
            .split_once('-')
            .with_context(|| format!("`{token}` should look like 8-1230"))?;
        let (start, end) = (parse_loose_time(start)?, parse_loose_time(end)?);
        if start >= end {
            bail!("`{token}`: start must be before end");
        }
        blocks.push((start, end));
    }
    if blocks.is_empty() {
        bail!("enter at least one block, e.g. 8-1230 1300-1700");
    }
    blocks.sort();
    if blocks.windows(2).any(|w| w[0].1 > w[1].0) {
        bail!("blocks overlap");
    }
    Ok(blocks
        .into_iter()
        .map(|(s, e)| format!("{}-{}", fmt_time(s), fmt_time(e)))
        .collect())
}

pub fn fmt_time(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

/// How a proposed entry relates to what is already in SMB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    New,
    Exists,
    Conflict(String),
}

/// Compares a proposed entry with the entries SMB already holds for that week.
pub fn classify(entry: &Entry, existing: &[Entry]) -> Status {
    if existing.contains(entry) {
        return Status::Exists;
    }
    match existing.iter().find(|e| e.overlaps(entry)) {
        Some(e) => Status::Conflict(format!("{}-{}", fmt_time(e.start), fmt_time(e.end))),
        None => Status::New,
    }
}

mod hhmm {
    use chrono::NaiveTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &NaiveTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::fmt_time(*t))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveTime, D::Error> {
        let s = String::deserialize(d)?;
        super::parse_time(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(line: &str) -> Entry {
        Entry::parse_line(line).unwrap()
    }

    #[test]
    fn parses_and_formats_lines() {
        let entry = e("2026-10-23 08:00-12:30");
        assert_eq!(entry.to_line(), "2026-10-23 08:00-12:30");
    }

    #[test]
    fn parses_loose_day_blocks() {
        let want = vec!["08:00-12:30".to_string(), "13:00-17:00".to_string()];
        assert_eq!(parse_day_blocks("8-1230 1300-1700").unwrap(), want);
        assert_eq!(
            parse_day_blocks("08:00 - 12:30, 13:00 – 17:00").unwrap(),
            want
        );
        assert_eq!(parse_day_blocks("13-17 and 8-12.30").unwrap(), want);
        assert!(parse_day_blocks("9-17 12-13").is_err());
        assert!(parse_day_blocks("17-9").is_err());
        assert!(parse_day_blocks("").is_err());
        assert!(parse_day_blocks("25-26").is_err());
    }

    #[test]
    fn rejects_reversed_times() {
        assert!(Entry::parse_line("2026-10-23 13:00-12:00").is_err());
    }

    #[test]
    fn classifies_against_existing() {
        let existing = vec![e("2026-10-23 08:00-12:30"), e("2026-10-23 13:00-17:00")];
        assert_eq!(
            classify(&e("2026-10-23 08:00-12:30"), &existing),
            Status::Exists
        );
        assert_eq!(
            classify(&e("2026-10-23 12:30-13:00"), &existing),
            Status::New
        );
        assert_eq!(
            classify(&e("2026-10-24 08:00-12:30"), &existing),
            Status::New
        );
        assert!(matches!(
            classify(&e("2026-10-23 09:00-10:00"), &existing),
            Status::Conflict(_)
        ));
    }
}
