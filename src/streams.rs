//! Persistent radio registrations and bounded, offline playlist parsing.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};

pub const MAX_ENTRIES: usize = 1000;
const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub name: String,
}

impl Entry {
    pub fn validated(&self) -> Result<Self> {
        let mut url = url::Url::parse(self.url.trim()).context("Expected an HTTP(S) stream URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "Expected an HTTP(S) stream URL"
        );
        ensure!(
            url.username().is_empty() && url.password().is_none(),
            "Authenticated stream URLs are not supported"
        );
        ensure!(url.as_str().len() <= 8192, "Stream URL is too long");
        url.set_fragment(None);
        let name = self.name.trim();
        ensure!(
            !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control),
            "Channel name must contain 1–512 bytes without control characters"
        );
        Ok(Self {
            url: url.into(),
            name: name.into(),
        })
    }
    pub fn track(&self) -> crate::model::Track {
        crate::model::Track {
            id: uuid::Uuid::new_v4().to_string(),
            playback: crate::model::PlaybackSource::Stream {
                url: self.url.clone(),
            },
            title: self.name.clone(),
            artist: String::new(),
            album: String::new(),
            track_number: 0,
            duration_ms: None,
            cover: None,
            video: false,
            source: None,
        }
    }
}

pub fn is_playlist(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["m3u", "m3u8", "pls"]
            .iter()
            .any(|v| e.eq_ignore_ascii_case(v))
    })
}

pub fn read_playlist(path: &Path) -> Result<Vec<Entry>> {
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "Playlist must be a regular file"
    );
    let file = std::fs::File::open(path).context("Cannot open stream playlist")?;
    let mut text = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .context("Playlist must be UTF-8 text")?;
    ensure!(text.len() as u64 <= MAX_BYTES, "Playlist exceeds 1 MiB");
    parse_playlist(&text)
}

pub fn parse_playlist(text: &str) -> Result<Vec<Entry>> {
    ensure!(text.len() as u64 <= MAX_BYTES, "Playlist exceeds 1 MiB");
    let text = text.trim_start_matches('\u{feff}');
    ensure!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("#EXT-X-")),
        "This is an HLS manifest. Register its HTTP(S) URL as one stream instead"
    );
    let mut entries = Vec::new();
    if text
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("[playlist]")
    {
        let mut fields: BTreeMap<usize, (Option<String>, Option<String>)> = BTreeMap::new();
        let mut declared = None;
        for (line_no, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with([';', '#', '[']) {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("Line {}: expected key=value", line_no + 1))?;
            let key = key.trim().to_ascii_lowercase();
            if key == "numberofentries" {
                declared = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("Invalid NumberOfEntries")?,
                );
                continue;
            }
            let field = if let Some(n) = key.strip_prefix("file") {
                Some((n, false))
            } else {
                key.strip_prefix("title").map(|n| (n, true))
            };
            if let Some((n, title)) = field {
                let n = n.parse::<usize>().context("Invalid PLS entry number")?;
                ensure!((1..=MAX_ENTRIES).contains(&n), "Invalid PLS entry number");
                let pair = fields.entry(n).or_default();
                let slot = if title { &mut pair.1 } else { &mut pair.0 };
                ensure!(
                    slot.is_none(),
                    "Duplicate PLS field at line {}",
                    line_no + 1
                );
                *slot = Some(value.trim().to_owned());
            }
        }
        for (n, (url, name)) in fields {
            let url = url.with_context(|| format!("PLS entry {n}: missing File{n}"))?;
            entries.push(
                Entry {
                    name: name.unwrap_or_else(|| url.clone()),
                    url,
                }
                .validated()
                .with_context(|| format!("PLS entry {n}"))?,
            );
        }
        ensure!(
            declared.is_none_or(|n| n == entries.len()),
            "PLS NumberOfEntries does not match its entries"
        );
    } else {
        let mut name = None;
        for (line_no, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(info) = line.strip_prefix("#EXTINF:") {
                ensure!(
                    name.is_none(),
                    "Line {}: previous channel has no URL",
                    line_no + 1
                );
                name = Some(
                    info.split_once(',')
                        .context("EXTINF requires a channel name")?
                        .1
                        .trim()
                        .to_owned(),
                );
            } else if !line.starts_with('#') {
                entries.push(
                    Entry {
                        url: line.into(),
                        name: name.take().unwrap_or_else(|| line.into()),
                    }
                    .validated()
                    .with_context(|| format!("Line {}", line_no + 1))?,
                );
                ensure!(
                    entries.len() <= MAX_ENTRIES,
                    "Playlist exceeds 1000 entries"
                );
            }
        }
        if name.is_some() {
            bail!("Last channel has no URL");
        }
    }
    ensure!(
        !entries.is_empty() && entries.len() <= MAX_ENTRIES,
        "Playlist must contain 1–1000 channels"
    );
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn site_exports_preserve_names_and_stable_urls() {
        let m3u = "\u{feff}#EXTM3U\r\n#EXTINF:-1,KBS 음악\r\nhttps://radio.bsod.kr/stream?stn=kbs&ch=1fm\r\n";
        let pls = "[playlist]\nNumberOfEntries=1\nFile1=https://radio.bsod.kr/stream?stn=kbs&ch=1fm\nTitle1=KBS 음악\nLength1=-1\nVersion=2";
        assert_eq!(parse_playlist(m3u).unwrap(), parse_playlist(pls).unwrap());
    }
    #[test]
    fn rejects_segments_partial_entries_and_local_paths() {
        for text in [
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\na.ts",
            "#EXTINF:-1,Radio",
            "file:///tmp/a.mp3",
            "[playlist]\nTitle1=Radio",
            "https://example.com/a\n/tmp/file",
            "[playlist]\nNumberOfEntries=2\nFile1=https://example.com/a",
        ] {
            assert!(parse_playlist(text).is_err(), "{text}");
        }
    }
}
