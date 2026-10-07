//! Portable library archives. Video remuxing uses installed FFmpeg; no network access.
mod media;
mod progress;
mod restore;
pub use progress::Progress;
pub(crate) use progress::Tracker;
#[cfg(test)]
mod tests;
pub(crate) use restore::{Publication, prepare, recover};

use crate::{
    library::Record, metadata::Metadata, model::Track, platform::Paths, store::Store, streams,
};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_normalization::UnicodeNormalization;

const FORMAT_VERSION: u32 = 2;
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;
const MAX_TRACKS: usize = 100_000;
const MAX_REPORTS: usize = 1_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SavedMetadata {
    pub automatic: Metadata,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl SavedMetadata {
    fn effective(track: &Track) -> Self {
        Self {
            automatic: Metadata {
                title: track.title.clone(),
                artist: track.artist.clone(),
                method: "archive".into(),
                warning: None,
            },
            title: Some(track.title.clone()),
            artist: Some(track.artist.clone()),
            album: Some(track.album.clone()),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Catalog {
    pub records: Vec<Record>,
    pub metadata: BTreeMap<String, SavedMetadata>,
    pub streams: Vec<streams::Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    // Paths and IDs are archive-relative here, never trusted destination paths.
    track: Track,
    metadata: SavedMetadata,
    audio: Asset,
    original_sha256: String,
    video: Option<Asset>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    tracks: Vec<Entry>,
    streams: Vec<streams::Entry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub operation: String,
    pub status: String,
    pub job_id: Option<String>,
    pub included: usize,
    pub videos: usize,
    pub radios: usize,
    pub added: usize,
    pub duplicates: usize,
    pub warning_count: usize,
    pub reports: Vec<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
}

impl Report {
    fn note(&mut self, message: String) {
        self.warning_count += 1;
        if self.reports.len() < MAX_REPORTS {
            // Keep status replies bounded below the wire limit.
            self.reports.push(message.chars().take(2_048).collect());
        }
    }
}

fn check_stop(stop: &AtomicBool) -> Result<()> {
    ensure!(
        !stop.load(Ordering::Relaxed),
        "Archive operation interrupted"
    );
    Ok(())
}

/// Detect in-place changes while reading; callers also verify second-pass hashes.
fn hash_file(path: &Path, stop: &AtomicBool, progress: &mut Tracker<'_>) -> Result<(u64, String)> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let before = file.metadata()?;
    ensure!(
        before.is_file(),
        "Expected a regular file: {}",
        path.display()
    );
    let (bytes, hash) = hash_reader(&mut file, stop, progress)?;
    let after = file.metadata()?;
    ensure!(
        before.len() == bytes && after.len() == bytes && before.modified()? == after.modified()?,
        "File changed while reading: {}",
        path.display()
    );
    Ok((bytes, hash))
}

fn hash_reader(
    reader: &mut impl Read,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<(u64, String)> {
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        check_stop(stop)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        progress.advance(count);
        bytes = bytes
            .checked_add(count as u64)
            .context("File is too large")?;
    }
    Ok((bytes, format!("{:x}", digest.finalize())))
}

fn asset(
    path: &Path,
    name: String,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<Asset> {
    let (bytes, sha256) = hash_file(path, stop, progress)?;
    Ok(Asset {
        path: name,
        bytes,
        sha256,
    })
}

fn copy_audio(
    source: &Path,
    destination: &Path,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<(String, u64)> {
    let mut input =
        File::open(source).with_context(|| format!("Cannot read {}", source.display()))?;
    let before = input.metadata()?;
    ensure!(before.is_file(), "Audio must be a regular file");
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0; 128 * 1024];
    loop {
        check_stop(stop)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
        count += n as u64;
        progress.advance(n);
    }
    let after = input.metadata()?;
    ensure!(
        before.len() == count && after.len() == count && before.modified()? == after.modified()?,
        "Audio changed while copying: {}",
        source.display()
    );
    Ok((
        format!("{:x}", hash.finalize()),
        tar_time(before.modified()?)?,
    ))
}

fn tar_time(time: SystemTime) -> Result<u64> {
    Ok(time
        .duration_since(UNIX_EPOCH)
        .context("File timestamp predates the Unix epoch")?
        .as_secs())
}

/// Snapshot all metadata first, then stream bounded chunks of the actual assets.
pub fn export(paths: &Paths, output: &Path) -> Result<Report> {
    export_tracked(paths, output, &mut Tracker::silent())
}

pub fn export_with_progress(
    paths: &Paths,
    output: &Path,
    sink: &mut dyn FnMut(&Progress),
) -> Result<Report> {
    export_tracked(paths, output, &mut Tracker::new(sink))
}

fn export_tracked(paths: &Paths, output: &Path, progress: &mut Tracker<'_>) -> Result<Report> {
    progress.begin("snapshot", 0, None);
    let stop = AtomicBool::new(false);
    ensure!(
        !output.try_exists()?,
        "Output already exists: {}",
        output.display()
    );
    let catalog = Store::archive_snapshot(&paths.database())?;
    ensure!(
        catalog.records.len() <= MAX_TRACKS,
        "Too many tracks for library archive"
    );
    let mut manifest = Manifest {
        format: "vtamp-library".into(),
        version: FORMAT_VERSION,
        tracks: vec![],
        streams: catalog.streams,
    };
    let mut inputs = BTreeMap::new();
    let mut mtimes = BTreeMap::new();
    let mut names = HashSet::from(["manifest.json".to_owned()]);
    let mut report = Report {
        operation: "export".into(),
        status: "completed".into(),
        radios: manifest.streams.len(),
        ..Default::default()
    };
    let stage = tempfile::tempdir()?;
    let mut originals = Vec::new();
    let mut videos = Vec::new();
    for record in &catalog.records {
        let video = if record.track.source.is_some() {
            let dir = crate::deletion::managed_path(paths, &record.track)?;
            let path = dir.join("video.mkv");
            path.try_exists()?.then_some(path)
        } else {
            None
        };
        videos.push(video);
    }
    let tools = if videos.iter().any(Option::is_some) {
        Some(media::Tools::load(paths)?)
    } else {
        None
    };
    progress.begin("copying", catalog.records.len(), None);
    for (index, record) in catalog.records.iter().enumerate() {
        progress.item(index, &record.track.title);
        let name = readable_name(&record.track, &mut names);
        let source = record
            .track
            .playback
            .file()
            .context("Expected a file track")?;
        let extension = source
            .extension()
            .and_then(|s| s.to_str())
            .context("Audio has no extension")?;
        let audio_name = format!("{name}.{extension}");
        let copied = stage.path().join(&audio_name);
        let (original_sha256, mtime) = copy_audio(source, &copied, &stop, progress)?;
        mtimes.insert(audio_name.clone(), mtime);
        originals.push((name, audio_name, copied, original_sha256));
    }
    progress.end();
    progress.begin("tagging", catalog.records.len(), None);
    for (index, record) in catalog.records.iter().enumerate() {
        check_stop(&stop)?;
        progress.item(index, &record.track.title);
        let cover = match &record.track.cover {
            Some(path) if path.try_exists()? => Some(media::Cover::read(path)?),
            Some(path) => {
                report.note(format!("Cover is missing: {}", path.display()));
                None
            }
            None => None,
        };
        media::tag_audio(&originals[index].2, &record.track, cover.as_ref())
            .with_context(|| format!("Cannot embed tags and artwork: {}", record.track.title))?;
        media::embedded_cover(&originals[index].2)
            .with_context(|| format!("Cannot read embedded artwork: {}", record.track.title))?;
    }
    progress.end();
    if let Some(tools) = &tools {
        progress.begin("muxing", videos.iter().flatten().count(), None);
        let mut done = 0;
        for (index, video) in videos.iter().enumerate() {
            if let Some(video) = video {
                let (name, _, audio, _) = &originals[index];
                mtimes.insert(
                    format!("{name}.mkv"),
                    tar_time(video.metadata()?.modified()?)?,
                );
                progress.item(done, &catalog.records[index].track.title);
                tools.mux(
                    video,
                    audio,
                    &stage.path().join(format!("{name}.mkv")),
                    &stop,
                    progress,
                )?;
                done += 1;
            }
        }
        progress.end();
    }
    progress.begin("hashing", catalog.records.len(), None);
    for (index, (record, (name, audio_name, audio_path, original_sha256))) in
        catalog.records.into_iter().zip(originals).enumerate()
    {
        progress.item(index, &record.track.title);
        let audio = asset(&audio_path, audio_name, &stop, progress)?;
        inputs.insert(audio.path.clone(), audio_path);
        let video = if videos[index].is_some() {
            let video_name = format!("{name}.mkv");
            let video_path = stage.path().join(&video_name);
            let asset = asset(&video_path, video_name, &stop, progress)?;
            inputs.insert(asset.path.clone(), video_path);
            report.videos += 1;
            Some(asset)
        } else {
            None
        };
        let mut track = record.track;
        let metadata = catalog
            .metadata
            .get(&track.id)
            .cloned()
            .unwrap_or_else(|| SavedMetadata::effective(&track));
        track.id = index.to_string();
        track.playback = crate::model::PlaybackSource::File {
            path: audio.path.clone().into(),
        };
        track.cover = None;
        manifest.tracks.push(Entry {
            track,
            metadata,
            audio,
            original_sha256,
            video,
        });
        report.included += 1;
    }
    progress.end();
    validate(&manifest)?;
    let json = serde_json::to_vec(&manifest)?;
    ensure!(
        json.len() as u64 <= MAX_MANIFEST,
        "Archive manifest exceeds 64 MiB"
    );
    let parent = output.parent().context("Output has no parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    let total = manifest
        .tracks
        .iter()
        .flat_map(assets)
        .try_fold(0u64, |n, a| n.checked_add(a.bytes))
        .context("Archive size overflow")?;
    let count = manifest.tracks.iter().flat_map(assets).count();
    progress.begin("compressing", count, Some(total));
    {
        let encoder = GzEncoder::new(temp.as_file_mut(), Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        append(
            &mut tar,
            "manifest.json",
            json.len() as u64,
            tar_time(SystemTime::now())?,
            json.as_slice(),
        )?;
        for (index, a) in manifest.tracks.iter().flat_map(assets).enumerate() {
            progress.item(index, &a.path);
            let path = &inputs[&a.path];
            let mut reader = CheckingReader {
                inner: File::open(path)?,
                hash: Sha256::new(),
                bytes: 0,
                progress,
            };
            append(&mut tar, &a.path, a.bytes, mtimes[&a.path], &mut reader)?;
            ensure!(
                reader.bytes == a.bytes && format!("{:x}", reader.hash.finalize()) == a.sha256,
                "File changed during export: {}",
                path.display()
            );
            ensure!(
                reader.inner.metadata()?.len() == a.bytes,
                "File size changed during export"
            );
        }
        tar.into_inner()?.finish()?;
    }
    progress.end();
    progress.begin("finalizing", 0, None);
    temp.as_file_mut().sync_all()?;
    temp.persist_noclobber(output).map_err(|e| e.error)?;
    File::open(parent)?.sync_all()?;
    progress.begin("completed", 0, None);
    report.progress = Some(progress.value.clone());
    Ok(report)
}

struct CheckingReader<'a, 'b, R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
    progress: &'a mut Tracker<'b>,
}
impl<R: Read> Read for CheckingReader<'_, '_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.hash.update(&buffer[..n]);
        self.bytes += n as u64;
        self.progress.advance(n);
        Ok(n)
    }
}

fn append<W: Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    bytes: u64,
    mtime: u64,
    reader: impl Read,
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_size(bytes);
    header.set_mode(0o600);
    header.set_mtime(mtime);
    header.set_cksum();
    tar.append_data(&mut header, name, reader)?;
    Ok(())
}

fn assets(entry: &Entry) -> impl Iterator<Item = &Asset> {
    std::iter::once(&entry.audio).chain(&entry.video)
}

/// Keep directly extracted files useful, including on case-insensitive filesystems.
/// Leave room for a collision suffix and a media extension within a ustar name field.
fn readable_name(track: &Track, used: &mut HashSet<String>) -> String {
    let artist = track.artist.trim();
    let title = track.title.trim();
    let title = if title.is_empty() { "Untitled" } else { title };
    let label = if artist.is_empty() || artist.eq_ignore_ascii_case("Unknown artist") {
        title.to_owned()
    } else {
        format!("{artist} - {title}")
    };
    let clean: String = label
        .nfc()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = clean.trim_matches([' ', '.']);
    let mut bytes = 0;
    let mut base: String = clean
        .chars()
        .take_while(|c| {
            bytes += c.len_utf8();
            bytes <= 80
        })
        .collect();
    base = base.trim_end_matches([' ', '.']).to_owned();
    if base.is_empty() {
        base = "Untitled".into();
    }
    let device = base.split('.').next().unwrap().to_ascii_uppercase();
    if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && matches!(device.as_bytes()[3], b'1'..=b'9'))
    {
        base.insert(0, '_');
    }
    for number in 1.. {
        let candidate = if number == 1 {
            base.clone()
        } else {
            format!("{base} ({number})")
        };
        if used.insert(crate::library::normalized(&candidate)) {
            return candidate;
        }
    }
    unreachable!()
}

fn safe_asset_path(path: &str) -> bool {
    let parts: Vec<_> = Path::new(path).components().collect();
    !path.is_empty()
        && path.len() < 256
        && !path.contains('\\')
        && !path.contains('/')
        && parts.len() == 1
        && parts.iter().all(|c| matches!(c, Component::Normal(_)))
        && !path.starts_with('.')
        && crate::library::normalized(&parts[0].as_os_str().to_string_lossy()) != "manifest.json"
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn validate(manifest: &Manifest) -> Result<BTreeMap<String, Asset>> {
    ensure!(
        manifest.format == "vtamp-library" && matches!(manifest.version, 1 | FORMAT_VERSION),
        "Unsupported library archive format"
    );
    ensure!(
        manifest.tracks.len() <= MAX_TRACKS && manifest.streams.len() <= MAX_TRACKS,
        "Too many archive entries"
    );
    let mut files = BTreeMap::new();
    let mut videos = HashSet::new();
    for entry in &manifest.tracks {
        ensure!(
            entry.track.playback.file().is_some(),
            "Expected a file track"
        );
        ensure!(
            valid_hash(&entry.original_sha256),
            "Invalid original audio checksum"
        );
        for text in [
            &entry.track.title,
            &entry.track.artist,
            &entry.track.album,
            &entry.metadata.automatic.title,
            &entry.metadata.automatic.artist,
        ] {
            ensure!(
                text.len() <= 8192 && !text.chars().any(char::is_control),
                "Invalid track metadata"
            );
        }
        for text in [
            &entry.metadata.title,
            &entry.metadata.artist,
            &entry.metadata.album,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                text.len() <= 8192 && !text.chars().any(char::is_control),
                "Invalid metadata override"
            );
        }
        if let Some(source) = &entry.track.source {
            source.validate()?;
            ensure!(
                manifest.version >= 2 || source.range.is_none(),
                "Time ranges require archive version 2"
            );
            ensure!(
                crate::youtube::valid_id(&source.video_id) && videos.insert(source.key()),
                "Invalid or duplicate YouTube identity"
            );
            ensure!(
                Path::new(&entry.audio.path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("m4a")),
                "YouTube audio must be included as m4a"
            );
            let source_manifest = crate::imports::Manifest {
                track_id: "00000000-0000-0000-0000-000000000000".into(),
                source: source.clone(),
                metadata: entry.metadata.automatic.clone(),
                title_override: entry.metadata.title.clone(),
                artist_override: entry.metadata.artist.clone(),
            };
            ensure!(
                serde_json::to_vec_pretty(&source_manifest)?.len() < 64 * 1024,
                "YouTube source manifest exceeds 64 KiB"
            );
        } else {
            ensure!(entry.video.is_none(), "Video requires a YouTube source");
        }
        ensure!(
            crate::library::supported(Path::new(&entry.audio.path)),
            "Unsupported archive audio"
        );
        if let Some(video) = &entry.video {
            ensure!(
                Path::new(&video.path)
                    .extension()
                    .is_some_and(|ext| ext == "mkv"),
                "Archive video must be MKV"
            );
        }
        for a in assets(entry) {
            ensure!(
                safe_asset_path(&a.path) && valid_hash(&a.sha256),
                "Invalid archive asset"
            );
            ensure!(
                files.insert(a.path.clone(), a.clone()).is_none(),
                "Duplicate archive asset path"
            );
        }
    }
    for stream in &manifest.streams {
        stream.validated()?;
    }
    Ok(files)
}

/// Do not use tar::unpack: the manifest is an allowlist, and links are never accepted.
fn extract(
    path: &Path,
    destination: &Path,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<Manifest> {
    progress.begin("reading_manifest", 0, None);
    let decoder = GzDecoder::new(File::open(path)?);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries()?;
    let mut first = entries.next().context("Archive is empty")??;
    ensure!(
        first.header().entry_type().is_file()
            && first.path()?.as_ref() == Path::new("manifest.json"),
        "Archive must begin with manifest.json"
    );
    ensure!(
        first.size() <= MAX_MANIFEST,
        "Archive manifest exceeds 64 MiB"
    );
    let manifest: Manifest = serde_json::from_reader(&mut first)?;
    let mut expected = validate(&manifest)?;
    let total = expected
        .values()
        .try_fold(0u64, |n, a| n.checked_add(a.bytes))
        .context("Archive size overflow")?;
    progress.begin("extracting", expected.len(), Some(total));
    for (index, item) in entries.enumerate() {
        check_stop(stop)?;
        let mut item = item?;
        ensure!(
            item.header().entry_type().is_file(),
            "Archive links and special entries are forbidden"
        );
        let name = item
            .path()?
            .to_str()
            .context("Archive paths must be UTF-8")?
            .to_owned();
        let a = expected
            .remove(&name)
            .context("Unexpected or duplicate archive file")?;
        progress.item(index, &name);
        ensure!(item.size() == a.bytes, "Archive file size mismatch");
        let target = destination.join(&name);
        fs::create_dir_all(target.parent().unwrap())?;
        let mut output = File::options().write(true).create_new(true).open(&target)?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 128 * 1024];
        let mut count = 0;
        loop {
            check_stop(stop)?;
            let n = item.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
            count += n as u64;
            progress.advance(n);
        }
        ensure!(
            count == a.bytes && format!("{:x}", digest.finalize()) == a.sha256,
            "Archive checksum mismatch: {name}"
        );
        output.sync_all()?;
    }
    ensure!(expected.is_empty(), "Archive is missing declared files");
    // Consume the gzip trailer too: truncated/corrupt trailers must fail before publication.
    let mut decoder = archive.into_inner();
    let mut padding = [0; 4096];
    loop {
        check_stop(stop)?;
        let n = decoder.read(&mut padding)?;
        if n == 0 {
            break;
        }
        ensure!(
            padding[..n].iter().all(|b| *b == 0),
            "Unexpected data after tar terminator"
        );
    }
    ensure!(decoder.get_ref().metadata()?.len() > 0, "Empty archive");
    progress.end();
    progress.begin("validating", manifest.tracks.len(), None);
    for (index, entry) in manifest.tracks.iter().enumerate() {
        progress.item(index, &entry.track.title);
        check_stop(stop)?;
        crate::library::read_track(
            &destination.join(&entry.audio.path),
            index.to_string(),
            &destination.join(".probe-covers"),
        )
        .context("Invalid archived audio")?;
        media::embedded_cover(&destination.join(&entry.audio.path))?;
    }
    progress.end();
    Ok(manifest)
}

pub fn preview(paths: &Paths, archive: &Path) -> Result<Report> {
    preview_tracked(paths, archive, &mut Tracker::silent())
}

pub fn preview_with_progress(
    paths: &Paths,
    archive: &Path,
    sink: &mut dyn FnMut(&Progress),
) -> Result<Report> {
    preview_tracked(paths, archive, &mut Tracker::new(sink))
}

fn preview_tracked(paths: &Paths, archive: &Path, progress: &mut Tracker<'_>) -> Result<Report> {
    progress.begin("snapshot", 0, None);
    let snapshot = Store::archive_snapshot(&paths.database())?;
    let stage = tempfile::tempdir()?;
    let stop = AtomicBool::new(false);
    let manifest = extract(archive, stage.path(), &stop, progress)?;
    media::video_tools(paths, &manifest, stage.path(), &stop, progress)?;
    let (mut report, _) = restore::plan(&manifest, &snapshot, &stop, progress)?;
    report.operation = "dry_run".into();
    progress.begin("completed", 0, None);
    report.progress = Some(progress.value.clone());
    Ok(report)
}
