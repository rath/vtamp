//! Move folder originals through the OS Trash so they can be restored by the user.
use super::{matching_entries, without_entries};
use crate::{
    audio::PlaybackBackend,
    engine::Engine,
    library,
    model::ApiError,
    platform::{self, Paths},
    source,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[cfg(target_os = "macos")]
fn system_trash(path: &Path) -> Result<()> {
    use objc2_foundation::{NSFileManager, NSURL};
    let url = NSURL::from_file_path(path).context("Invalid audio file URL")?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| anyhow::anyhow!("Cannot move file to Trash: {error}"))
}

#[cfg(not(target_os = "macos"))]
fn system_trash(path: &Path) -> Result<()> {
    trash::delete(path).context("Cannot move file to Trash")
}

// Pass the original path to the OS Trash service. Record the pending change
// before moving it so a crash between the move and SQLite commit is recoverable.
// Keep queue IDs because symlink aliases stop resolving after the move.
#[derive(Serialize, Deserialize)]
struct Pending {
    id: String,
    audio: PathBuf,
    entries: HashSet<String>,
    parent_dev: u64,
    parent_ino: u64,
}

pub(super) fn move_with<B: PlaybackBackend>(
    paths: &Paths,
    store: &mut Store,
    engine: &mut Engine<B>,
    id: &str,
    move_file: impl FnOnce(&Path) -> Result<()>,
) -> Result<serde_json::Value> {
    let record = store
        .record(id)?
        .ok_or_else(|| ApiError::new("track_not_found", "Library track not found"))?;
    let audio = record
        .track
        .playback
        .file()
        .filter(|_| record.track.source.is_none())
        .ok_or_else(|| {
            ApiError::new(
                "not_folder_track",
                "Trash requires an audio file in a registered folder",
            )
        })?;
    let data = paths.data.canonicalize()?;
    ensure!(
        library::supported(audio)
            && audio.canonicalize()? == audio
            && store.roots()?.iter().any(|root| audio.starts_with(root))
            && !source::PROVIDERS
                .iter()
                .any(|provider| audio.starts_with(data.join("imports").join(provider))),
        "Track is outside its registered folder or managed separately"
    );
    let metadata = fs::symlink_metadata(audio)?;
    ensure!(
        metadata.is_file()
            && metadata.len() == record.bytes
            && metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos() == record.modified,
        "File changed since the last scan. Rescan the Library before moving it to Trash."
    );
    let parent = audio.parent().context("Missing audio parent")?.metadata()?;
    let pending = Pending {
        id: id.to_owned(),
        audio: audio.to_owned(),
        entries: matching_entries(&engine.state, id, audio),
        parent_dev: parent.dev(),
        parent_ino: parent.ino(),
    };
    let next = without_entries(&engine.state, &pending.entries);
    let tx = store.prepare_track_delete(id, &next)?;
    let dir = paths.data.join("pending-trash");
    platform::private_dir(&dir)?;
    let journal = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    platform::atomic_json(&journal, &pending)?;
    fs::File::open(&dir)?.sync_all()?;
    // Resolving a large queue and syncing the journal can take time. Check
    // the directory entry again immediately before handing the path to the OS.
    if !fs::symlink_metadata(audio).is_ok_and(|now| {
        now.is_file()
            && now.dev() == metadata.dev()
            && now.ino() == metadata.ino()
            && now.len() == metadata.len()
            && now.modified().ok() == metadata.modified().ok()
    }) || audio.canonicalize().as_deref().ok() != Some(audio)
    {
        fs::remove_file(journal)?;
        anyhow::bail!("File changed before the Trash move. Rescan the Library and try again.");
    }
    if let Err(error) = move_file(audio) {
        // Keep the journal on an uncertain outcome. Recovery never moves a file.
        if audio.try_exists().unwrap_or(false) {
            fs::remove_file(journal)?;
        }
        return Err(error);
    }
    tx.commit()
        .context("File moved to Trash. Restart the server to finish the Library update")?;
    engine.accept_library_delete(next);
    let warning = fs::remove_file(journal)
        .err()
        .map(|e| format!("File moved to Trash. Journal cleanup will retry on startup: {e}"));
    Ok(serde_json::json!({"trashed": id, "warning": warning}))
}

pub(crate) fn move_track<B: PlaybackBackend>(
    paths: &Paths,
    store: &mut Store,
    engine: &mut Engine<B>,
    id: &str,
) -> Result<serde_json::Value> {
    move_with(paths, store, engine, id, system_trash)
}

pub(crate) fn recover(paths: &Paths, store: &mut Store) -> Result<()> {
    let dir = paths.data.join("pending-trash");
    if !dir.try_exists()? {
        return Ok(());
    }
    super::directory(&dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let journal = entry.path();
        if journal.extension().is_none_or(|ext| ext != "json") || !entry.file_type()?.is_file() {
            continue;
        }
        let pending: Pending = serde_json::from_slice(&fs::read(&journal)?)?;
        let track = store.track(&pending.id)?;
        ensure!(
            track.as_ref().is_none_or(
                |track| track.source.is_none() && track.playback.file() == Some(&pending.audio)
            ),
            "Mismatched pending Trash entry"
        );
        // A missing or unmounted volume is not evidence of a completed move.
        let parent = pending.audio.parent().context("Missing audio parent")?;
        if !parent
            .metadata()
            .is_ok_and(|m| m.dev() == pending.parent_dev && m.ino() == pending.parent_ino)
        {
            tracing::warn!(
                "Trash recovery deferred until {} is available",
                parent.display()
            );
            continue;
        }
        match fs::symlink_metadata(&pending.audio) {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let state = store.restore()?;
                let mut entries = pending.entries;
                if track.is_some() {
                    entries.extend(matching_entries(&state, &pending.id, &pending.audio));
                    store
                        .prepare_track_delete(&pending.id, &without_entries(&state, &entries))?
                        .commit()?;
                } else {
                    // A scan after an uncertain move can remove the catalog row
                    // while leaving the previously matched queue entries intact.
                    let next = without_entries(&state, &entries);
                    if next.revision != state.revision {
                        store.save(&next)?;
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
        fs::remove_file(journal)?;
    }
    Ok(())
}
