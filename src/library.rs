use crate::model::{ScanWarning, Track};
use anyhow::{Context, Result, bail};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::Accessor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub track: Track,
    pub modified: u128,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub records: Vec<Record>,
    pub warnings: Vec<String>,
    pub warning_count: usize,
    pub warning_details: Vec<ScanWarning>,
}

impl Scan {
    fn warn(&mut self, path: Option<PathBuf>, message: String) {
        self.warning_count += 1;
        if self.warnings.len() < 100 {
            self.warnings.push(match &path {
                Some(path) => format!("{}: {message}", path.display()),
                None => message.clone(),
            });
            self.warning_details.push(ScanWarning { path, message });
        }
    }
}

pub fn scan_summary(scan: &Scan, old: &[Record]) -> crate::model::ScanSummary {
    let before: HashMap<_, _> = old.iter().map(|r| (&r.track.id, r)).collect();
    let after: HashSet<_> = scan.records.iter().map(|r| &r.track.id).collect();
    let mut summary = crate::model::ScanSummary {
        removed: before.keys().filter(|id| !after.contains(**id)).count(),
        warning_count: scan.warning_count,
        warnings: scan.warning_details.clone(),
        ..Default::default()
    };
    for record in &scan.records {
        match before.get(&record.track.id) {
            None => summary.added += 1,
            Some(old)
                if old.modified == record.modified
                    && old.bytes == record.bytes
                    && old.track == record.track =>
            {
                summary.unchanged += 1
            }
            Some(_) => summary.updated += 1,
        }
    }
    summary
}

pub fn normalized(text: &str) -> String {
    text.nfkc().flat_map(char::to_lowercase).collect()
}

/// The normalized title/artist/album text used by library search. The TUI's
/// queue filter applies the same text so both lists match the same way.
pub fn search_blob(track: &Track) -> String {
    normalized(&format!("{} {} {}", track.title, track.artist, track.album))
}

fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

pub fn supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "m4a" | "mp4" | "aac" | "mp3" | "flac" | "wav" | "ogg"
        )
    })
}

pub fn scan(paths: &[PathBuf], old: &[Record], cache: &Path) -> Scan {
    let previous: HashMap<_, _> = old
        .iter()
        .filter_map(|r| r.track.playback.file().map(|path| (path.to_path_buf(), r)))
        .collect();
    let mut seen = HashSet::new();
    let mut result = Scan::default();
    for root in paths {
        for entry in WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                e.file_name() != ".staging"
                    && !e.file_name().to_string_lossy().starts_with(".replaced-")
            })
        {
            match entry {
                Ok(entry) if entry.file_type().is_file() && supported(entry.path()) => {
                    let read = (|| -> Result<Option<Record>> {
                        let path = entry.path().canonicalize()?;
                        if !seen.insert(path.clone()) {
                            return Ok(None);
                        }
                        let metadata = path.metadata()?;
                        let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
                        if let Some(record) = previous.get(&path)
                            && record.modified == modified
                            && record.bytes == metadata.len()
                            && record.track.cover.as_ref().is_none_or(|p| p.exists())
                        {
                            // The sidecar can appear or vanish without touching
                            // the audio file, so re-check it on every scan.
                            let mut record = (*record).clone();
                            record.track.video = crate::video::sidecar(&record.track).is_some();
                            return Ok(Some(record));
                        }
                        let id = previous
                            .get(&path)
                            .map(|r| r.track.id.clone())
                            .unwrap_or_else(|| Uuid::new_v4().to_string());
                        let mut track = read_track(&path, id, cache)?;
                        if path.file_name().is_some_and(|n| n == "audio.m4a")
                            && let Some(parent) = path.parent()
                            && let Ok(manifest) = crate::imports::read_manifest(parent)
                        {
                            track.id = manifest.track_id;
                            track.title =
                                manifest.title_override.unwrap_or(manifest.metadata.title);
                            track.artist =
                                manifest.artist_override.unwrap_or(manifest.metadata.artist);
                            track.source = Some(manifest.source);
                            track.apply_source_album();
                        }
                        track.video = crate::video::sidecar(&track).is_some();
                        Ok(Some(Record {
                            track,
                            modified,
                            bytes: metadata.len(),
                        }))
                    })();
                    match read {
                        Ok(Some(record)) => result.records.push(record),
                        Ok(None) => (),
                        Err(error) => {
                            result.warn(Some(entry.path().to_path_buf()), format!("{error:#}"));
                            // Keep an already indexed file if a transient read fails.
                            if let Ok(path) = entry.path().canonicalize()
                                && let Some(old) = previous.get(&path)
                            {
                                result.records.push((*old).clone());
                            }
                        }
                    }
                }
                Err(error) => {
                    result.warn(error.path().map(Path::to_path_buf), error.to_string());
                    // A disconnected volume must not erase its catalog.
                    if let Some(path) = error.path() {
                        for record in old.iter().filter(|r| {
                            r.track
                                .playback
                                .file()
                                .is_some_and(|file| file.starts_with(path))
                        }) {
                            if seen.insert(record.track.playback.file().unwrap().to_path_buf()) {
                                result.records.push(record.clone());
                            }
                        }
                    }
                }
                _ => (),
            }
        }
    }
    result.records.sort_by(|a, b| {
        (
            &a.track.artist,
            &a.track.album,
            a.track.track_number,
            a.track.playback.file(),
        )
            .cmp(&(
                &b.track.artist,
                &b.track.album,
                b.track.track_number,
                b.track.playback.file(),
            ))
    });
    result
}

pub fn read_track(path: &Path, id: String, cache: &Path) -> Result<Track> {
    // Lofty's extended-size atom skipping rejects some otherwise valid m4a files.
    // A dedicated MP4 reader handles that layout without rewriting the source.
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "m4a" | "mp4"))
    {
        return read_mp4(path, id, cache);
    }
    let tagged = lofty::read_from_path(path)
        .with_context(|| format!("Cannot read audio metadata for {}", path.display()))?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let title = tag
        .and_then(|t| t.title())
        .map(|s| clean(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| clean(&path.file_stem().unwrap_or_default().to_string_lossy()));
    let artist = tag
        .and_then(|t| t.artist())
        .map(|s| clean(&s))
        .unwrap_or_else(|| "Unknown artist".into());
    let album = tag
        .and_then(|t| t.album())
        .map(|s| clean(&s))
        .unwrap_or_default();
    let track_number = tag.and_then(|t| t.track()).unwrap_or(0);
    let mut cover = None;
    if let Some(picture) = tag.and_then(|t| {
        t.pictures()
            .iter()
            .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
            .or_else(|| t.pictures().first())
    }) && picture.data().len() <= 16 * 1024 * 1024
    {
        let destination = cache.join(format!("{id}.png"));
        if let Ok(image) = decode_image(picture.data()) {
            fs::create_dir_all(cache)?;
            if image.thumbnail(512, 512).save(&destination).is_ok() {
                cover = Some(destination);
            }
        }
    }
    if cover.is_none()
        && let Some(parent) = path.parent()
    {
        cover = [
            "cover.jpg",
            "cover.png",
            "cover.jpeg",
            "folder.jpg",
            "folder.png",
            "Folder.jpg",
            "Cover.jpg",
        ]
        .iter()
        .map(|name| parent.join(name))
        .find(|p| p.is_file());
    }
    Ok(Track {
        id,
        playback: crate::model::PlaybackSource::File { path: path.into() },
        title,
        artist,
        album,
        track_number,
        duration_ms: Some(tagged.properties().duration().as_millis() as u64),
        cover,
        video: false,
        source: None,
    })
}

fn read_mp4(path: &Path, id: String, cache: &Path) -> Result<Track> {
    let tag = mp4ameta::Tag::read_from_path(path).context("Cannot read MP4 metadata")?;
    let mut cover = None;
    if let Some(art) = tag.artwork()
        && let Ok(image) = decode_image(art.data)
    {
        fs::create_dir_all(cache)?;
        let destination = cache.join(format!("{id}.png"));
        if image.thumbnail(512, 512).save(&destination).is_ok() {
            cover = Some(destination);
        }
    }
    if cover.is_none()
        && let Some(parent) = path.parent()
    {
        cover = [
            "cover.jpg",
            "cover.png",
            "cover.jpeg",
            "folder.jpg",
            "folder.png",
            "Folder.jpg",
            "Cover.jpg",
        ]
        .iter()
        .map(|name| parent.join(name))
        .find(|p| p.is_file());
    }
    Ok(Track {
        id,
        playback: crate::model::PlaybackSource::File { path: path.into() },
        title: tag
            .title()
            .map(clean)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| clean(&path.file_stem().unwrap_or_default().to_string_lossy())),
        artist: tag
            .artist()
            .map(clean)
            .unwrap_or_else(|| "Unknown artist".into()),
        album: tag.album().map(clean).unwrap_or_default(),
        track_number: tag.track_number().unwrap_or(0).into(),
        duration_ms: Some(tag.duration().as_millis() as u64),
        cover,
        video: false,
        source: None,
    })
}

pub fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("Cover image exceeds 16 MiB");
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_normalizes_korean_and_case() {
        assert_eq!(normalized("가"), normalized("가"));
        assert_eq!(normalized("ＭUSIC"), "music");
    }
    #[test]
    fn damaged_file_does_not_abort_scan() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("broken.m4a"), b"not music").unwrap();
        let result = scan(&[dir.path().to_path_buf()], &[], dir.path());
        assert!(result.records.is_empty());
        assert_eq!(result.warnings.len(), 1);
    }
}
