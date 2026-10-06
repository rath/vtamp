use super::*;
use crate::{model::PlaybackSource, platform, youtube};
use std::time::UNIX_EPOCH;
use uuid::Uuid;

pub(crate) struct RestoredRecord {
    pub record: Record,
    pub metadata: SavedMetadata,
}

pub(crate) struct Publication {
    pub records: Vec<RestoredRecord>,
    pub roots: Vec<PathBuf>,
    pub streams: Vec<streams::Entry>,
    pub report: Report,
    stage: PathBuf,
    journal: Journal,
}

#[derive(Serialize, Deserialize)]
struct Directory {
    id: String,
    video_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    receipt: String,
    directories: Vec<Directory>,
}

impl Directory {
    fn destination(&self, paths: &Paths) -> Result<PathBuf> {
        ensure!(
            Uuid::parse_str(&self.id).is_ok(),
            "Invalid restore identity"
        );
        Ok(if let Some(video_id) = &self.video_id {
            ensure!(
                youtube::valid_id(video_id),
                "Invalid restore video identity"
            );
            paths.data.join("imports/youtube").join(video_id)
        } else {
            paths.data.join("imports/archive").join(&self.id)
        })
    }
}

/// Entries selected for restoration, in manifest order.
type Selected = Vec<usize>;

pub(super) fn plan(
    manifest: &Manifest,
    catalog: &Catalog,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<(Report, Selected)> {
    let mut report = Report {
        operation: "import".into(),
        status: "completed".into(),
        ..Default::default()
    };
    let mut hashes = HashSet::new();
    let mut videos = HashSet::new();
    progress.begin("checking_library", catalog.records.len(), None);
    for (index, record) in catalog.records.iter().enumerate() {
        progress.item(index, &record.track.title);
        check_stop(stop)?;
        let file = record
            .track
            .playback
            .file()
            .context("Expected a file record")?;
        // Existing identities are preserved, including temporarily unavailable files.
        if let Some(source) = &record.track.source {
            videos.insert(source.video_id.clone());
        }
        if let Ok((_, hash)) = hash_file(file, stop, progress) {
            hashes.insert(hash);
        }
    }
    progress.end();
    progress.begin("planning", manifest.tracks.len(), None);
    let mut selected = vec![];
    for (index, entry) in manifest.tracks.iter().enumerate() {
        progress.item(index, &entry.track.title);
        check_stop(stop)?;
        let title = &entry.track.title;
        if let Some(source) = &entry.track.source
            && !videos.insert(source.video_id.clone())
        {
            report.duplicates += 1;
            report.note(format!("Skipped existing YouTube track: {title}"));
            continue;
        }
        if entry.track.source.is_none() {
            if hashes.contains(&entry.original_sha256) || hashes.contains(&entry.audio.sha256) {
                report.duplicates += 1;
                report.note(format!("Skipped existing audio: {title}"));
                continue;
            }
            hashes.insert(entry.original_sha256.clone());
            hashes.insert(entry.audio.sha256.clone());
        }
        report.included += 1;
        report.videos += usize::from(entry.video.is_some());
        report.added += 1;
        selected.push(index);
    }
    let mut urls: HashSet<_> = catalog.streams.iter().map(|s| s.url.clone()).collect();
    for stream in &manifest.streams {
        let stream = stream.validated()?;
        if urls.insert(stream.url) {
            report.radios += 1;
        } else {
            report.duplicates += 1;
            report.note(format!("Skipped existing radio: {}", stream.name));
        }
    }
    progress.end();
    Ok((report, selected))
}

pub(crate) fn prepare(
    paths: &Paths,
    archive: &Path,
    catalog: Catalog,
    stop: &AtomicBool,
    progress: &mut Tracker<'_>,
) -> Result<Publication> {
    platform::private_dir(&paths.data.join("archives"))?;
    let staging = paths.data.join("archives/.staging");
    platform::private_dir(&staging)?;
    let temp = tempfile::Builder::new()
        .prefix("restore-")
        .tempdir_in(&staging)?;
    let extracted = temp.path().join("extracted");
    fs::create_dir(&extracted)?;
    let manifest = extract(archive, &extracted, stop, progress)?;
    let tools = media::video_tools(paths, &manifest, &extracted, stop, progress)?;
    let (report, selected) = plan(&manifest, &catalog, stop, progress)?;
    let mut records = vec![];
    let mut directories = vec![];
    let mut roots = vec![];
    progress.begin("preparing", selected.len(), None);
    for (done, index) in selected.into_iter().enumerate() {
        check_stop(stop)?;
        let entry = &manifest.tracks[index];
        progress.item(done, &entry.track.title);
        let id = Uuid::new_v4().to_string();
        let dir = Directory {
            id: id.clone(),
            video_id: entry.track.source.as_ref().map(|s| s.video_id.clone()),
        };
        let destination = dir.destination(paths)?;
        let staged = temp.path().join(&id);
        fs::create_dir(&staged)?;
        ensure!(
            !destination.try_exists()?,
            "Restore destination already exists: {}",
            destination.display()
        );
        let extension = Path::new(&entry.audio.path)
            .extension()
            .and_then(|s| s.to_str())
            .context("Missing audio extension")?;
        let name = if entry.track.source.is_some() {
            "audio.m4a".to_owned()
        } else {
            format!("audio.{extension}")
        };
        fs::rename(extracted.join(&entry.audio.path), staged.join(&name))?;
        roots.push(destination.clone());
        let actual_audio = staged.join(&name);
        let mut track = entry.track.clone();
        track.id = id.clone();
        track.playback = PlaybackSource::File {
            path: destination.join(&name),
        };
        track.cover = None;
        if let Some(cover) = media::embedded_cover(&actual_audio)? {
            let name = cover.save(&staged, track.source.is_some())?;
            track.cover = Some(destination.join(name));
        }
        if let Some(video) = &entry.video {
            progress.begin("restoring_video", 1, None);
            progress.item(0, &track.title);
            tools.as_ref().context("Video tools unavailable")?.silent(
                &extracted.join(&video.path),
                &staged.join("video.mkv"),
                stop,
                progress,
            )?;
            track.video = true;
            progress.end();
            progress.begin("preparing", report.added, None);
            progress.item(done, &track.title);
        }
        if let Some(source) = &track.source {
            let manifest = crate::imports::Manifest {
                track_id: id.clone(),
                source: source.clone(),
                metadata: entry.metadata.automatic.clone(),
                title_override: entry.metadata.title.clone(),
                artist_override: entry.metadata.artist.clone(),
            };
            platform::atomic_json(&staged.join("source.json"), &manifest)?;
            // Enforce the same source constraints as subsequent scans/deletions.
            crate::imports::read_manifest(&staged)?;
        } else {
            platform::atomic_json(&staged.join(".archive-owner.json"), &id)?;
        }
        let metadata = actual_audio.metadata()?;
        records.push(RestoredRecord {
            record: Record {
                track,
                modified: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
                bytes: metadata.len(),
            },
            metadata: entry.metadata.clone(),
        });
        File::open(&staged)?.sync_all()?;
        directories.push(dir);
    }
    progress.end();
    let mut urls: HashSet<_> = catalog.streams.iter().map(|s| s.url.clone()).collect();
    let mut streams = vec![];
    for stream in &manifest.streams {
        let stream = stream.validated()?;
        if urls.insert(stream.url.clone()) {
            streams.push(stream);
        }
    }
    let journal = Journal {
        receipt: format!("/archive/{}", Uuid::new_v4()),
        directories,
    };
    platform::atomic_json(&temp.path().join("journal.json"), &journal)?;
    File::open(temp.path())?.sync_all()?;
    File::open(&staging)?.sync_all()?;
    File::open(paths.data.join("archives"))?.sync_all()?;
    File::open(&paths.data)?.sync_all()?;
    Ok(Publication {
        records,
        roots,
        streams,
        report,
        stage: temp.keep(),
        journal,
    })
}

impl Publication {
    pub(crate) fn receipt(&self) -> &str {
        &self.journal.receipt
    }

    /// Only new, owned directories are installed. The journal precedes every rename.
    pub(crate) fn publish(
        &mut self,
        paths: &Paths,
        stop: &AtomicBool,
        progress: &mut Tracker<'_>,
    ) -> Result<()> {
        progress.begin("publishing", self.journal.directories.len(), None);
        for (index, dir) in self.journal.directories.iter().enumerate() {
            progress.item(index, &self.records[index].record.track.title);
            check_stop(stop)?;
            platform::private_dir(&paths.data.join("imports"))?;
            let destination = dir.destination(paths)?;
            let parent = destination.parent().unwrap();
            platform::private_dir(parent)?;
            File::open(paths.data.join("imports"))?.sync_all()?;
            File::open(&paths.data)?.sync_all()?;
            let parent = parent.canonicalize()?;
            let destination = parent.join(destination.file_name().unwrap());
            ensure!(
                !destination.try_exists()?,
                "Restore destination already exists"
            );
            // Empty directories created by this operation act as no-clobber reservations.
            fs::create_dir(&destination)?;
            if let Err(error) = fs::rename(self.stage.join(&dir.id), &destination) {
                let _ = fs::remove_dir(&destination);
                return Err(error.into());
            }
            File::open(&parent)?.sync_all()?;
        }
        for item in &mut self.records {
            let file = item.record.track.playback.file().unwrap().canonicalize()?;
            item.record.track.playback = PlaybackSource::File { path: file };
            if let Some(cover) = &mut item.record.track.cover {
                *cover = cover.canonicalize()?;
            }
        }
        for root in &mut self.roots {
            *root = root.canonicalize()?;
        }
        progress.end();
        Ok(())
    }

    pub(crate) fn finish(&self, paths: &Paths, committed: bool) -> Result<()> {
        if !committed {
            rollback(paths, &self.journal)?;
        }
        fs::remove_dir_all(&self.stage)?;
        File::open(self.stage.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}

fn rollback(paths: &Paths, journal: &Journal) -> Result<()> {
    for dir in &journal.directories {
        let destination = dir.destination(paths)?;
        for parent in [
            paths.data.join("imports"),
            destination.parent().unwrap().to_path_buf(),
        ] {
            if parent.try_exists()? {
                ensure!(
                    fs::symlink_metadata(parent)?.file_type().is_dir(),
                    "Restore cleanup refuses symlinked parents"
                );
            }
        }
        if !destination.try_exists()? {
            continue;
        }
        ensure!(
            fs::symlink_metadata(&destination)?.file_type().is_dir(),
            "Restore cleanup refuses a symlink"
        );
        let owned = if dir.video_id.is_some() {
            crate::imports::read_manifest(&destination)
                .is_ok_and(|m| m.track_id == dir.id && Some(m.source.video_id) == dir.video_id)
        } else {
            fs::read(destination.join(".archive-owner.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<String>(&b).ok())
                .is_some_and(|id| id == dir.id)
        };
        if owned {
            fs::remove_dir_all(&destination)?;
        } else if fs::read_dir(&destination)?.next().is_none() {
            fs::remove_dir(&destination)?;
        } else {
            // A committed track may have been deleted and reimported before
            // deferred cleanup runs. Its replacement is not ours to remove.
            continue;
        }
        File::open(destination.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}

pub(crate) fn recover(paths: &Paths, store: &Store) -> Result<()> {
    let root = paths.data.join("archives/.staging");
    if !root.exists() {
        return store.clear_archive_receipts();
    }
    for path in [paths.data.join("archives"), root.clone()] {
        ensure!(
            fs::symlink_metadata(path)?.file_type().is_dir(),
            "Archive staging must be a real directory"
        );
    }
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_dir(),
            "Unexpected archive staging entry"
        );
        if !entry.file_name().to_string_lossy().starts_with("restore-") {
            continue;
        }
        let path = entry.path();
        let journal = path.join("journal.json");
        if journal.exists() {
            let bytes = fs::read(&journal)?;
            ensure!(
                bytes.len() as u64 <= MAX_MANIFEST,
                "Restore journal is too large"
            );
            let journal: Journal = serde_json::from_slice(&bytes)?;
            let id = journal
                .receipt
                .strip_prefix("/archive/")
                .context("Invalid restore receipt")?;
            ensure!(Uuid::parse_str(id).is_ok(), "Invalid restore receipt");
            // A durable transaction receipt remains authoritative even after
            // the user unregisters or deletes every restored track.
            if !store.archive_committed(&journal.receipt)? {
                rollback(paths, &journal)?;
            }
        }
        fs::remove_dir_all(path)?;
    }
    File::open(root)?.sync_all()?;
    store.clear_archive_receipts()
}
