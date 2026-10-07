use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// A source-relative interval. Saved media always starts at time zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub start_ms: u64,
    #[serde(default)]
    pub end_ms: Option<u64>,
}

pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<TimeRange>, D::Error> {
    TimeRange::normalized(Option::<TimeRange>::deserialize(deserializer)?)
        .map_err(serde::de::Error::custom)
}

impl TimeRange {
    pub fn validate(self) -> Result<()> {
        // FFmpeg uses signed microsecond timestamps internally.
        ensure!(
            self.start_ms <= i64::MAX as u64 / 1000,
            "Start time is too large"
        );
        if let Some(end) = self.end_ms {
            ensure!(end <= i64::MAX as u64 / 1000, "End time is too large");
            ensure!(end > self.start_ms, "End must be after start");
        }
        Ok(())
    }

    pub fn normalized(range: Option<Self>) -> Result<Option<Self>> {
        if let Some(range) = range {
            range.validate()?;
        }
        Ok(range.filter(|r| r.start_ms != 0 || r.end_ms.is_some()))
    }

    pub fn from_text(start: &str, end: &str) -> Result<Option<Self>> {
        Self::normalized(Some(Self {
            start_ms: if start.trim().is_empty() {
                0
            } else {
                parse_time(start).context("Start")?
            },
            end_ms: if end.trim().is_empty() {
                None
            } else {
                Some(parse_time(end).context("End")?)
            },
        }))
    }

    pub fn check_duration(self, duration: Option<f64>) -> Result<()> {
        self.validate()?;
        let duration = duration
            .filter(|n| n.is_finite() && *n > 0.0)
            .context("Cannot determine video length for a time range")?
            * 1000.0;
        ensure!(
            (self.start_ms as f64) < duration,
            "Start must be before the end of the video"
        );
        ensure!(
            self.end_ms.is_none_or(|end| end as f64 <= duration),
            "End exceeds the video length"
        );
        Ok(())
    }

    pub fn section(self) -> String {
        fn seconds(ms: u64) -> String {
            format!("{}.{:03}", ms / 1000, ms % 1000)
        }
        format!(
            "*{}-{}",
            seconds(self.start_ms),
            self.end_ms.map(seconds).unwrap_or_else(|| "inf".into())
        )
    }

    pub fn label(self) -> String {
        fn time(ms: u64) -> String {
            let seconds = ms / 1000;
            let mut text = if seconds >= 3600 {
                format!(
                    "{}:{:02}:{:02}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                )
            } else {
                format!("{:02}:{:02}", seconds / 60, seconds % 60)
            };
            if !ms.is_multiple_of(1000) {
                text.push_str(&format!(".{:03}", ms % 1000));
            }
            text
        }
        format!(
            "{}–{}",
            time(self.start_ms),
            self.end_ms.map(time).unwrap_or_else(|| "end".into())
        )
    }
}

/// Parse whole seconds, M:SS, or H:MM:SS without floating-point conversion.
pub fn parse_time(text: &str) -> Result<u64> {
    let parts: Vec<_> = text.trim().split(':').collect();
    ensure!(
        (1..=3).contains(&parts.len()),
        "Use seconds, M:SS or H:MM:SS"
    );
    let mut total = 0_u64;
    for (index, part) in parts.iter().enumerate() {
        ensure!(
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
            "Use seconds, M:SS or H:MM:SS"
        );
        let value: u64 = part.parse().context("Time is too large")?;
        if index > 0 && (part.len() != 2 || value >= 60) {
            bail!("Use two digits from 00 to 59 after ':'");
        }
        total = total
            .checked_mul(60)
            .and_then(|n| n.checked_add(value))
            .context("Time is too large")?;
    }
    let ms = total.checked_mul(1000).context("Time is too large")?;
    TimeRange {
        start_ms: ms,
        end_ms: None,
    }
    .validate()?;
    Ok(ms)
}

/// Stable directory/database identity; full downloads keep their old paths.
pub fn resource_key(video_id: &str, range: Option<TimeRange>) -> String {
    match range.filter(|r| r.start_ms != 0 || r.end_ms.is_some()) {
        None => video_id.to_owned(),
        Some(range) => format!(
            "{video_id}--{}-{}",
            range.start_ms,
            range
                .end_ms
                .map(|n| n.to_string())
                .unwrap_or_else(|| "end".into())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_normalization() {
        for text in ["83", "1:23", "0:01:23", " 01:23 "] {
            assert_eq!(parse_time(text).unwrap(), 83_000);
        }
        assert_eq!(parse_time("123:45").unwrap(), 7_425_000);
        for text in [
            "",
            "-1",
            "1.5",
            "1:60",
            "1:2",
            "1:60:00",
            "1:00:00:00",
            "18446744073709551615",
            "+1",
        ] {
            assert!(parse_time(text).is_err(), "{text}");
        }
        assert_eq!(TimeRange::from_text("", "").unwrap(), None);
        assert_eq!(TimeRange::from_text("0", "").unwrap(), None);
        for (start, end) in [("10", "10"), ("20", "10"), ("", "0")] {
            assert!(TimeRange::from_text(start, end).is_err());
        }
        let range = TimeRange::from_text("1:23", "2:45").unwrap().unwrap();
        assert_eq!(range.label(), "01:23–02:45");
        assert_eq!(range.section(), "*83.000-165.000");
        assert_eq!(
            resource_key("VIDEO000001", Some(range)),
            "VIDEO000001--83000-165000"
        );
        assert!(range.check_duration(Some(165.0)).is_ok());
        for duration in [None, Some(f64::NAN), Some(0.0), Some(83.0), Some(164.9)] {
            assert!(range.check_duration(duration).is_err());
        }
    }
}
