use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, NaiveTime};
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

    /// Parses `YYYY-MM-DD HH:MM-HH:MM`, the format used by the preview editor.
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

pub fn parse_time(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").with_context(|| format!("bad time `{s}`"))
}

pub fn fmt_time(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

/// How a proposed entry relates to what is already in SMB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Unknown,
    New,
    Exists,
    Conflict(String),
    Added,
    Failed(String),
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Unknown => "checking…".into(),
            Status::New => "new".into(),
            Status::Exists => "already logged".into(),
            Status::Conflict(c) => format!("overlaps {c}"),
            Status::Added => "added ✓".into(),
            Status::Failed(e) => format!("failed: {e}"),
        }
    }
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
