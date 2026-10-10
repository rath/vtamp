use super::{DownloadProgress, TimeRange};
use serde_json::Value;

/// yt-dlp and its FFmpeg downloader share stdout. Only completed FFmpeg
/// progress blocks produce updates; diagnostic stderr remains private.
pub(super) struct ProgressParser {
    range: Option<TimeRange>,
    current: DownloadProgress,
}

impl ProgressParser {
    pub fn new(range: Option<TimeRange>) -> Self {
        Self {
            range,
            current: DownloadProgress {
                processing_total_ms: range
                    .and_then(|r| r.end_ms.map(|end| end.saturating_sub(r.start_ms))),
                ..Default::default()
            },
        }
    }

    pub fn line(&mut self, line: &str) -> Option<DownloadProgress> {
        if let Some(json) = line.strip_prefix("VTAMP_PROGRESS ") {
            let value: Value = serde_json::from_str(json).ok()?;
            self.current.bytes = value["downloaded_bytes"].as_u64();
            self.current.total = value["total_bytes"]
                .as_u64()
                .or_else(|| value["total_bytes_estimate"].as_u64());
            // FFmpeg's final yt-dlp hook reports output bytes, not transfer
            // throughput. Keep media processing speed/ETA until this stage finishes.
            if self.current.processed_ms.is_none() {
                self.current.speed = positive(value["speed"].as_f64());
                self.current.eta = value["eta"].as_f64().filter(|n| n.is_finite() && *n >= 0.0);
            }
            return Some(self.current.clone());
        }
        let range = self.range?;
        if let Some(duration) = line.strip_prefix("VTAMP_DURATION ") {
            if range.end_ms.is_none() {
                self.current.processing_total_ms = serde_json::from_str::<f64>(duration)
                    .ok()
                    .and_then(|n| positive(Some(n)))
                    .map(|n| (n * 1000.0) as u64)
                    .and_then(|n| n.checked_sub(range.start_ms));
            }
        } else if let Some(value) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = value.parse::<i64>() {
                let ms = us.max(0) as u64 / 1000;
                self.current.processed_ms = Some(self.current.processed_ms.unwrap_or(0).max(ms));
            }
        } else if let Some(value) = line.strip_prefix("speed=") {
            self.current.processing_speed =
                positive(value.trim().trim_end_matches('x').parse().ok());
        } else if matches!(line, "progress=continue" | "progress=end")
            && self.current.processed_ms.is_some()
        {
            self.current.speed = None;
            self.current.eta = self
                .current
                .processing_total_ms
                .zip(self.current.processed_ms)
                .zip(self.current.processing_speed)
                .map(|((total, done), speed)| total.saturating_sub(done) as f64 / 1000.0 / speed)
                .filter(|n| n.is_finite());
            return Some(self.current.clone());
        }
        None
    }
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|n| n.is_finite() && *n > 0.0)
}

impl DownloadProgress {
    pub fn processing_summary(&self) -> Option<String> {
        let done = self.processed_ms?;
        let mut text = format!("Processed {}", crate::model::display_time(done));
        if let Some(total) = self.processing_total_ms.filter(|n| *n > 0) {
            text.push_str(&format!(
                " / {} · {:.0}%",
                crate::model::display_time(total),
                (done as f64 / total as f64 * 100.0).min(100.0)
            ));
        }
        if let Some(speed) = positive(self.processing_speed) {
            text.push_str(&format!(" · {speed:.1}x"));
        }
        if let Some(eta) = self.eta.filter(|n| n.is_finite() && *n >= 0.0) {
            text.push_str(&format!(" · ETA {eta:.0}s"));
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_progress_is_media_time_and_survives_the_final_transfer_hook() {
        let mut parser = ProgressParser::new(TimeRange::from_text("12:51", "16:30").unwrap());
        for line in [
            "frame=250",
            "out_time_us=84000000",
            "speed=1.50x",
            "total_size=1000000",
        ] {
            assert!(parser.line(line).is_none());
        }
        let result = parser.line("progress=continue").unwrap();
        assert_eq!(result.processed_ms, Some(84_000));
        assert_eq!(result.processing_total_ms, Some(219_000));
        assert_eq!(result.eta, Some(90.0));
        assert_eq!(result.speed, None);
        assert_eq!(result.bytes, None);
        assert_eq!(
            result.processing_summary().as_deref(),
            Some("Processed 1:24 / 3:39 · 38% · 1.5x · ETA 90s")
        );
        let result = parser
            .line("VTAMP_PROGRESS {\"downloaded_bytes\":1000000,\"speed\":999999,\"eta\":0}")
            .unwrap();
        assert_eq!(result.processed_ms, Some(84_000));
        assert_eq!(result.eta, Some(90.0));
        assert_eq!(result.speed, None);
    }

    #[test]
    fn open_ends_and_unknown_speed_do_not_invent_totals_or_eta() {
        let mut parser = ProgressParser::new(TimeRange::from_text("1:00", "").unwrap());
        for line in ["out_time_us=-12000", "speed=N/A", "VTAMP_DURATION null"] {
            parser.line(line);
        }
        let result = parser.line("progress=continue").unwrap();
        assert_eq!(result.processed_ms, Some(0));
        assert_eq!(result.processing_total_ms, None);
        assert_eq!(result.eta, None);
        for line in ["VTAMP_DURATION 90.5", "out_time_us=10000000", "speed=0.5x"] {
            parser.line(line);
        }
        let result = parser.line("progress=continue").unwrap();
        assert_eq!(result.processing_total_ms, Some(30_500));
        assert_eq!(result.eta, Some(41.0));
        for speed in ["NaN", "inf", "-2x", "0x"] {
            parser.line(&format!("speed={speed}"));
            assert_eq!(parser.line("progress=end").unwrap().eta, None);
        }
        parser.line("out_time_us=oops");
        parser.line("out_time_us=5000000");
        assert_eq!(
            parser.line("progress=end").unwrap().processed_ms,
            Some(10_000)
        );
        assert!(
            ProgressParser::new(None)
                .line("out_time_us=123000")
                .is_none()
        );
    }
}
