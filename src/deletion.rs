//! Delete only validated managed downloads; never registered local music.
use crate::{
    audio::PlaybackBackend,
    engine::Engine,
    imports,
    model::{ApiError, PlaybackStatus, State, Track},
    platform::Paths,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

fn directory(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "Managed path is not a real directory: {}",
        path.display()
    );
    Ok(())
}

fn root(paths: &Paths) -> Result<PathBuf> {
    let imports = paths.data.join("imports");
    directory(&imports)?;
    let root = imports.join("youtube");
    directory(&root)?;
    Ok(root.canonicalize()?)
}

fn validate_files(path: &Path) -> Result<imports::Manifest> {
    directory(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file()
                && matches!(
                    entry.file_name().to_str(),
                    Some("audio.m4a" | "cover.jpg" | "source.json" | "video.mkv")
                ),
            "Import contains an unexpected file; refusing deletion"
        );
    }
    let manifest = imports::read_manifest(path)?;
    ensure!(
        crate::youtube::valid_id(&manifest.source.video_id),
        "Invalid managed video ID"
    );
    Ok(manifest)
}

pub(crate) fn managed_path(paths: &Paths, track: &Track) -> Result<PathBuf> {
    let source = track.source.as_ref().ok_or_else(|| {
        ApiError::new(
            "not_managed",
            "Only managed YouTube downloads can be deleted; local files are kept",
        )
    })?;
    ensure!(
        crate::youtube::valid_id(&source.video_id),
        "Invalid video ID"
    );
    let destination = root(paths)?.join(&source.video_id);
    ensure!(
        track.playback.file() == Some(destination.join("audio.m4a").as_path()),
        "Track is not in its managed download directory"
    );
    let manifest = validate_files(&destination)?;
    ensure!(
        manifest.track_id == track.id && manifest.source.video_id == source.video_id,
        "Managed import identity mismatch"
    );
    Ok(destination)
}

fn cleanup(path: &Path) -> Result<()> {
    // Leave the identity manifest until last so interrupted cleanup is recoverable.
    for name in ["audio.m4a", "cover.jpg", "video.mkv", "source.json"] {
        match fs::remove_file(path.join(name)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    fs::remove_dir(path)?;
    Ok(())
}

fn is_download(track: &Track, id: &str, audio: &Path) -> bool {
    track.id == id
        || track.playback.file().is_some_and(|path| {
            path == audio || path.canonicalize().is_ok_and(|path| path == audio)
        })
}

fn without_download(state: &State, id: &str, audio: &Path) -> State {
    let removed: HashSet<_> = state
        .queue
        .iter()
        .filter(|item| is_download(&item.track, id, audio))
        .map(|item| item.id.as_str())
        .collect();
    let mut next = state.clone();
    next.queue
        .retain(|item| !removed.contains(item.id.as_str()));
    next.play_next.retain(|id| !removed.contains(id.as_str()));
    if let Some(cursor) = &state.queue_cursor
        && removed.contains(cursor.as_str())
    {
        next.queue_cursor = state
            .queue
            .iter()
            .take_while(|item| item.id != *cursor)
            .filter(|item| !removed.contains(item.id.as_str()))
            .last()
            .map(|item| item.id.clone());
    }
    let removed_direct = state
        .direct
        .as_ref()
        .is_some_and(|item| is_download(&item.track, id, audio));
    if removed_direct {
        next.direct = None;
    }
    let removed_current = state
        .current()
        .is_some_and(|item| is_download(&item.track, id, audio));
    if removed_current {
        next.current_id = None;
        next.queue_cursor = None;
        next.status = PlaybackStatus::Stopped;
        next.position_ms = 0;
        next.stream_status = None;
        next.scheduled_stop = None;
        next.last_error = None;
    }
    let queue_changed = next.queue_changed_since(state);
    if queue_changed {
        next.queue_revision += 1;
    }
    if queue_changed || removed_direct || removed_current {
        next.revision += 1;
    }
    next
}

pub fn delete<B: PlaybackBackend>(
    paths: &Paths,
    store: &mut Store,
    engine: &mut Engine<B>,
    id: &str,
) -> Result<serde_json::Value> {
    let track = store
        .track(id)?
        .ok_or_else(|| ApiError::new("track_not_found", "Library track not found"))?;
    let destination = managed_path(paths, &track)?;
    let audio = destination.join("audio.m4a");
    let next = without_download(&engine.state, id, &audio);
    let staging = paths.data.join("imports/.staging");
    crate::platform::private_dir(&staging)?;
    directory(&staging)?;
    let staged = staging.join(format!("delete-{}", uuid::Uuid::new_v4()));
    fs::rename(&destination, &staged).context("Cannot stage download for deletion")?;
    if let Err(error) = store.delete_import(id, &next) {
        fs::rename(&staged, &destination)
            .context("Cannot roll back download deletion; restart the server to recover")?;
        return Err(error);
    }
    engine.accept_library_delete(next);
    let warning = cleanup(&staged).err().map(|error| {
        format!("Library entry removed; file cleanup will retry on server start: {error}")
    });
    Ok(serde_json::json!({"deleted": id, "warning": warning}))
}

/// A reserved staging directory is a tiny filesystem journal. Before the DB
/// commit, restore its files; after the commit, finish deleting them.
pub fn recover(paths: &Paths, store: &Store) -> Result<()> {
    let staging = paths.data.join("imports/.staging");
    if !staging.try_exists()? {
        return Ok(());
    }
    directory(&paths.data.join("imports"))?;
    directory(&staging)?;
    for entry in fs::read_dir(&staging)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("delete-")) else {
            continue;
        };
        if uuid::Uuid::parse_str(suffix).is_err() {
            continue;
        }
        let staged = entry.path();
        directory(&staged)?;
        if fs::read_dir(&staged)?.next().is_none() {
            fs::remove_dir(&staged)?;
            continue;
        }
        let manifest = validate_files(&staged)?;
        let destination = root(paths)?.join(&manifest.source.video_id);
        if let Some(track) = store.track(&manifest.track_id)? {
            ensure!(
                track.playback.file() == Some(destination.join("audio.m4a").as_path()),
                "Cannot recover deletion with mismatched track path"
            );
            ensure!(
                !destination.try_exists()?,
                "Cannot recover deletion over an existing download"
            );
            fs::rename(&staged, destination)?;
        } else {
            cleanup(&staged)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        library::Record,
        model::{PlaybackSource, QueueItem, Repeat, ScheduledStop},
        youtube::Source,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct Silent {
        stops: Arc<AtomicUsize>,
    }
    impl PlaybackBackend for Silent {
        fn load(&mut self, _: &Path, _: u64, _: u8, _: bool) -> Result<()> {
            panic!("Deletion must not load output")
        }
        fn pause(&mut self) {
            panic!("Deletion must not pause output")
        }
        fn resume(&mut self) -> Result<()> {
            panic!("Deletion must not resume output")
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        fn seek(&mut self, _: u64) -> Result<()> {
            panic!("Deletion must not seek output")
        }
        fn volume(&mut self, _: u8) {
            panic!("Deletion must not change volume")
        }
        fn position(&self) -> u64 {
            0
        }
        fn finished(&self) -> bool {
            false
        }
    }
    fn engine(state: State) -> Engine<Silent> {
        Engine::new(state, Silent::default())
    }
    fn saved_state(paths: &Paths) -> serde_json::Value {
        let db = rusqlite::Connection::open(paths.database()).unwrap();
        let saved: String = db
            .query_row("SELECT json FROM session WHERE id=1", [], |r| r.get(0))
            .unwrap();
        serde_json::from_str(&saved).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, Paths, Store, Track) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: home.path().into(),
            runtime: home.path().join("run"),
            cache: home.path().join("cache"),
        };
        let folder = home.path().join("imports/youtube/0OeEx5SiRI0");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("audio.m4a"), b"test audio").unwrap();
        fs::write(folder.join("cover.jpg"), b"test cover").unwrap();
        let source = Source {
            video_id: "0OeEx5SiRI0".into(),
            ..Default::default()
        };
        let track = Track {
            id: "download".into(),
            playback: PlaybackSource::File {
                path: folder.join("audio.m4a").canonicalize().unwrap(),
            },
            title: "Track".into(),
            artist: "Artist".into(),
            album: String::new(),
            track_number: 0,
            duration_ms: Some(1000),
            cover: Some(folder.join("cover.jpg")),
            video: false,
            source: Some(source.clone()),
        };
        crate::platform::atomic_json(
            &folder.join("source.json"),
            &imports::Manifest {
                track_id: track.id.clone(),
                source,
                metadata: Default::default(),
                title_override: None,
                artist_override: None,
            },
        )
        .unwrap();
        let mut store = Store::open(&paths.database()).unwrap();
        store
            .replace_catalog(&[Record {
                track: track.clone(),
                modified: 0,
                bytes: 10,
            }])
            .unwrap();
        (home, paths, store, track)
    }

    #[test]
    fn deletes_only_managed_files_and_metadata_without_schema_change() {
        let (_home, paths, mut store, track) = fixture();
        let folder = track.playback.file().unwrap().parent().unwrap();
        let result = delete(&paths, &mut store, &mut engine(State::default()), &track.id).unwrap();
        assert!(result["warning"].is_null());
        assert!(!folder.exists());
        assert!(store.track(&track.id).unwrap().is_none());
        assert!(store.video_record("0OeEx5SiRI0").unwrap().is_none());
        assert!(
            crate::library::scan(&[paths.data.join("imports/youtube")], &[], &paths.cache)
                .records
                .is_empty()
        );
        let db = rusqlite::Connection::open(paths.database()).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            8
        );
    }

    #[test]
    fn rejects_local_files() {
        let (_home, paths, mut store, track) = fixture();
        let mut local = track.clone();
        local.source = None;
        local.id = "local".into();
        local.playback = PlaybackSource::File {
            path: paths.data.join("original.m4a"),
        };
        fs::write(local.playback.file().unwrap(), b"original").unwrap();
        store
            .replace_catalog(&[Record {
                track: local.clone(),
                modified: 0,
                bytes: 8,
            }])
            .unwrap();
        assert!(delete(&paths, &mut store, &mut engine(State::default()), &local.id).is_err());
        assert_eq!(
            fs::read(local.playback.file().unwrap()).unwrap(),
            b"original"
        );
    }

    #[test]
    fn removes_all_queue_copies_and_aliases_without_interrupting_other_playback() {
        for direct in [false, true] {
            let (_home, paths, mut store, track) = fixture();
            let mut other = track.clone();
            other.id = "other".into();
            other.playback = PlaybackSource::File {
                path: paths.data.join("other.m4a"),
            };
            let first = QueueItem::new(other.clone());
            let last = QueueItem::new(other.clone());
            let mut alias = track.clone();
            alias.id = "path-alias".into();
            let alias_path = paths.data.join("alias.m4a");
            std::os::unix::fs::symlink(track.playback.file().unwrap(), &alias_path).unwrap();
            alias.playback = PlaybackSource::File { path: alias_path };
            let mut same_id = track.clone();
            same_id.playback = other.playback.clone();
            let deleted = QueueItem::new(track.clone());
            let current = if direct {
                QueueItem::new(other)
            } else {
                first.clone()
            };
            let state = State {
                queue: vec![
                    first.clone(),
                    deleted.clone(),
                    QueueItem::new(track.clone()),
                    QueueItem::new(alias),
                    QueueItem::new(same_id),
                    last.clone(),
                ],
                current_id: Some(current.id.clone()),
                direct: direct.then(|| Box::new(current.clone())),
                queue_cursor: direct.then(|| deleted.id.clone()),
                play_next: vec![deleted.id, last.id.clone()],
                position_ms: 9876,
                status: PlaybackStatus::Playing,
                shuffle: true,
                repeat: Repeat::One,
                volume: 13,
                scheduled_stop: Some(ScheduledStop::Deadline { deadline_ms: 12345 }),
                revision: 41,
                queue_revision: 12,
                ..Default::default()
            };
            store.save(&state).unwrap();
            let stops = Arc::default();
            let mut engine = Engine::new(
                state.clone(),
                Silent {
                    stops: Arc::clone(&stops),
                },
            );
            delete(&paths, &mut store, &mut engine, &track.id).unwrap();
            assert_eq!(engine.state.queue, vec![first.clone(), last.clone()]);
            assert_eq!(engine.state.current(), Some(&current));
            assert_eq!(engine.state.queue_cursor, direct.then(|| first.id.clone()));
            assert_eq!(engine.state.play_next, vec![last.id]);
            assert_eq!(engine.state.position_ms, state.position_ms);
            assert_eq!(engine.state.status, state.status);
            assert_eq!(engine.state.scheduled_stop, state.scheduled_stop);
            assert_eq!(engine.state.volume, state.volume);
            assert_eq!(engine.state.repeat, state.repeat);
            assert!(engine.state.shuffle);
            assert_eq!(engine.state.revision, state.revision + 1);
            assert_eq!(engine.state.queue_revision, state.queue_revision + 1);
            assert_eq!(stops.load(Ordering::SeqCst), 0);
            assert_eq!(
                saved_state(&paths),
                serde_json::to_value(&engine.state).unwrap()
            );
            assert!(!track.playback.file().unwrap().exists());
        }
    }

    #[test]
    fn deleting_the_current_download_stops_queue_or_direct_output() {
        for direct in [false, true] {
            for status in [
                PlaybackStatus::Playing,
                PlaybackStatus::Paused,
                PlaybackStatus::Stopped,
            ] {
                let (_home, paths, mut store, track) = fixture();
                let mut copy = track.clone();
                copy.id = "path-alias".into();
                let queued = QueueItem::new(copy.clone());
                let current = if direct {
                    QueueItem::new(copy)
                } else {
                    queued.clone()
                };
                let mut other = track.clone();
                other.id = "other".into();
                other.playback = PlaybackSource::File {
                    path: paths.data.join("other.m4a"),
                };
                let retained = QueueItem::new(other);
                let state = State {
                    queue: vec![queued, QueueItem::new(track.clone()), retained.clone()],
                    current_id: Some(current.id.clone()),
                    direct: direct.then(|| Box::new(current.clone())),
                    position_ms: 4321,
                    status,
                    scheduled_stop: Some(ScheduledStop::AfterCurrent {
                        queue_item_id: current.id,
                    }),
                    ..Default::default()
                };
                store.save(&state).unwrap();
                let stops = Arc::default();
                let mut engine = Engine::new(
                    state,
                    Silent {
                        stops: Arc::clone(&stops),
                    },
                );
                delete(&paths, &mut store, &mut engine, &track.id).unwrap();
                assert_eq!(engine.state.queue, vec![retained]);
                assert!(engine.state.direct.is_none());
                assert!(engine.state.current_id.is_none());
                assert!(engine.state.scheduled_stop.is_none());
                assert_eq!(engine.state.status, PlaybackStatus::Stopped);
                assert_eq!(engine.state.position_ms, 0);
                assert_eq!(stops.load(Ordering::SeqCst), 1);
                assert_eq!(
                    saved_state(&paths),
                    serde_json::to_value(&engine.state).unwrap()
                );
            }
        }
    }

    #[test]
    fn rejects_unexpected_files_symlinks_and_mismatched_manifests() {
        let (_home, paths, mut store, track) = fixture();
        let folder = track.playback.file().unwrap().parent().unwrap();
        let extra = folder.join("personal.txt");
        fs::write(&extra, b"keep").unwrap();
        assert!(delete(&paths, &mut store, &mut engine(State::default()), &track.id).is_err());
        assert!(extra.exists());
        fs::remove_file(&extra).unwrap();
        fs::remove_file(folder.join("cover.jpg")).unwrap();
        std::os::unix::fs::symlink(track.playback.file().unwrap(), folder.join("cover.jpg"))
            .unwrap();
        assert!(delete(&paths, &mut store, &mut engine(State::default()), &track.id).is_err());
        fs::remove_file(folder.join("cover.jpg")).unwrap();
        let mut manifest = imports::read_manifest(folder).unwrap();
        manifest.track_id = "different".into();
        crate::platform::atomic_json(&folder.join("source.json"), &manifest).unwrap();
        assert!(delete(&paths, &mut store, &mut engine(State::default()), &track.id).is_err());
        assert!(track.playback.file().unwrap().exists());
    }

    #[test]
    fn database_failure_restores_files_catalog_queue_and_playback() {
        for trigger in [
            "CREATE TRIGGER fail_delete BEFORE DELETE ON track_metadata BEGIN SELECT RAISE(ABORT,'injected'); END;",
            "CREATE TRIGGER fail_session BEFORE UPDATE ON session BEGIN SELECT RAISE(ABORT,'injected'); END;",
        ] {
            let (_home, paths, mut store, track) = fixture();
            let current = QueueItem::new(track.clone());
            let state = State {
                current_id: Some(current.id.clone()),
                queue: vec![current.clone(), QueueItem::new(track.clone())],
                status: PlaybackStatus::Playing,
                position_ms: 1234,
                play_next: vec![current.id],
                ..Default::default()
            };
            store.save(&state).unwrap();
            let db = rusqlite::Connection::open(paths.database()).unwrap();
            db.execute_batch(trigger).unwrap();
            let stops = Arc::default();
            let mut engine = Engine::new(
                state.clone(),
                Silent {
                    stops: Arc::clone(&stops),
                },
            );
            assert!(delete(&paths, &mut store, &mut engine, &track.id).is_err());
            assert!(track.playback.file().unwrap().exists());
            assert!(store.track(&track.id).unwrap().is_some());
            assert!(store.video_record("0OeEx5SiRI0").unwrap().is_some());
            assert_eq!(
                serde_json::to_value(&engine.state).unwrap(),
                serde_json::to_value(&state).unwrap()
            );
            assert_eq!(saved_state(&paths), serde_json::to_value(&state).unwrap());
            assert_eq!(stops.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn startup_recovers_deletion_on_either_side_of_database_commit() {
        for committed in [false, true] {
            let (_home, paths, mut store, track) = fixture();
            let staging = paths.data.join("imports/.staging");
            fs::create_dir_all(&staging).unwrap();
            let staged = staging.join(format!("delete-{}", uuid::Uuid::new_v4()));
            fs::rename(track.playback.file().unwrap().parent().unwrap(), &staged).unwrap();
            if committed {
                store.delete_import(&track.id, &State::default()).unwrap();
            }
            recover(&paths, &store).unwrap();
            assert!(!staged.exists());
            assert_eq!(track.playback.file().unwrap().exists(), !committed);
            assert_eq!(store.track(&track.id).unwrap().is_some(), !committed);
        }
    }
}
