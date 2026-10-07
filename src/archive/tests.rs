use super::*;
use crate::{
    library,
    model::{PlaybackSource, QueueItem, State},
    platform,
};
use std::time::UNIX_EPOCH;

fn empty() -> (tempfile::TempDir, Paths, Store) {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths {
        data: home.path().canonicalize().unwrap(),
        runtime: home.path().join("run"),
        cache: home.path().join("covers"),
    };
    let store = Store::open(&paths.database()).unwrap();
    (home, paths, store)
}

fn record(file: &Path, id: &str, paths: &Paths) -> Record {
    let metadata = file.metadata().unwrap();
    Record {
        track: library::read_track(file, id.into(), &paths.cache).unwrap(),
        modified: metadata
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        bytes: metadata.len(),
    }
}

fn fixture() -> (tempfile::TempDir, Paths, Store, PathBuf) {
    let (home, paths, mut store) = empty();
    let youtube = paths.data.join("imports/youtube/VIDEO000001");
    fs::create_dir_all(&youtube).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a");
    fs::copy(&fixture, youtube.join("audio.m4a")).unwrap();
    image::RgbImage::from_pixel(8, 8, image::Rgb([50, 60, 70]))
        .save(youtube.join("cover.jpg"))
        .unwrap();
    let manifest = crate::imports::Manifest {
        track_id: "original-youtube".into(),
        source: crate::youtube::Source {
            video_id: "VIDEO000001".into(),
            video_url: "https://www.youtube.com/watch?v=VIDEO000001".into(),
            original_title: "원래 제목".into(),
            music_album: Some("Original album".into()),
            ..Default::default()
        },
        metadata: Metadata {
            title: "Title".into(),
            artist: "Artist".into(),
            method: "rules".into(),
            warning: None,
        },
        title_override: None,
        artist_override: None,
    };
    platform::atomic_json(&youtube.join("source.json"), &manifest).unwrap();
    let mut downloaded = record(&youtube.join("audio.m4a"), &manifest.track_id, &paths);
    downloaded.track.source = Some(manifest.source);
    let local = paths.data.join("original.m4a");
    fs::copy(fixture, &local).unwrap();
    let local_record = record(&local, "original-local", &paths);
    store.replace_catalog(&[downloaded, local_record]).unwrap();
    store
        .edit_metadata(
            "original-youtube",
            Some("김동률 노래".into()),
            Some("김동률".into()),
            Some(String::new()),
            None,
        )
        .unwrap();
    store
        .edit_metadata(
            "original-local",
            Some("Local title".into()),
            None,
            Some("Local album".into()),
            None,
        )
        .unwrap();
    store
        .add_streams(&[streams::Entry {
            name: "음악 Radio".into(),
            url: "https://example.com/live".into(),
        }])
        .unwrap();
    (home, paths, store, local)
}

fn restore(paths: &Paths, store: &mut Store, archive: &Path) -> Report {
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        paths,
        archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    publication
        .publish(paths, &stop, &mut Tracker::silent())
        .unwrap();
    store.commit_archive(&publication).unwrap();
    publication.finish(paths, true).unwrap();
    publication.report
}

#[test]
fn archive_ranges_roundtrip_without_collapsing_full_or_other_excerpts() {
    let (_home, paths, mut store, _) = fixture();
    let original_dir = paths.data.join("imports/youtube/VIDEO000001");
    let original = crate::imports::read_manifest(&original_dir).unwrap();
    let mut records = store.records().unwrap();
    for (start, end) in [("1", "2"), ("3", "")] {
        let mut manifest = original.clone();
        manifest.track_id = format!("clip-{start}");
        manifest.source.range = crate::youtube::TimeRange::from_text(start, end).unwrap();
        let dir = paths
            .data
            .join("imports/youtube")
            .join(manifest.source.key());
        fs::create_dir(&dir).unwrap();
        fs::copy(original_dir.join("audio.m4a"), dir.join("audio.m4a")).unwrap();
        platform::atomic_json(&dir.join("source.json"), &manifest).unwrap();
        let mut clip = record(&dir.join("audio.m4a"), &manifest.track_id, &paths);
        clip.track.source = Some(manifest.source);
        records.push(clip);
    }
    store.replace_catalog(&records).unwrap();
    let archive = paths.data.join("clips.tar.gz");
    assert_eq!(export(&paths, &archive).unwrap().included, 4);
    let (_home, target, mut restored) = empty();
    assert_eq!(restore(&target, &mut restored, &archive).added, 4);
    let keys: HashSet<_> = restored
        .records()
        .unwrap()
        .iter()
        .filter_map(|r| r.track.source.as_ref().map(|s| s.key()))
        .collect();
    assert_eq!(
        keys,
        HashSet::from([
            "VIDEO000001".into(),
            "VIDEO000001--1000-2000".into(),
            "VIDEO000001--3000-end".into()
        ])
    );
    assert_eq!(restore(&target, &mut restored, &archive).added, 0);
    for record in restored.records().unwrap() {
        if record.track.source.is_some() {
            crate::deletion::managed_path(&target, &record.track).unwrap();
        }
    }
    let scan = library::scan(
        &restored.roots().unwrap(),
        &restored.records().unwrap(),
        &target.cache,
    );
    restored.replace_catalog(&scan.records).unwrap();
    assert_eq!(restored.records().unwrap().len(), 4);
    let downgraded = paths.data.join("invalid-v1.tar.gz");
    rewrite(&archive, &downgraded, |m| m.version = 1, false);
    assert!(preview(&target, &downgraded).is_err());
}

#[test]
fn archive_version_one_still_restores_full_downloads() {
    let (_home, paths, _store, _) = fixture();
    let archive = paths.data.join("current.tar.gz");
    export(&paths, &archive).unwrap();
    let legacy = paths.data.join("legacy.tar.gz");
    rewrite(&archive, &legacy, |m| m.version = 1, false);
    let (_home, target, mut restored) = empty();
    assert_eq!(restore(&target, &mut restored, &legacy).added, 2);
    assert!(restored.video_record("VIDEO000001").unwrap().is_some());
}

#[test]
fn archive_roundtrip_preserves_assets_overrides_rescans_and_session() {
    let (_source, paths, _store, local) = fixture();
    let archive = paths.data.join("library.tar.gz");
    let exported = export(&paths, &archive).unwrap();
    assert_eq!(
        (exported.included, exported.videos, exported.radios),
        (2, 0, 1)
    );
    assert!(export(&paths, &archive).is_err());
    let before = fs::read(&archive).unwrap();
    assert!(!before.is_empty());

    let (_target, target, mut store) = empty();
    let original = record(&local, "queue-only", &target).track;
    let state = State {
        queue: vec![QueueItem::new(original)],
        volume: 0,
        ..Default::default()
    };
    store.save(&state).unwrap();
    let report = restore(&target, &mut store, &archive);
    assert_eq!((report.added, report.radios), (2, 1));
    assert_eq!(
        serde_json::to_value(store.restore().unwrap()).unwrap(),
        serde_json::to_value(&state).unwrap()
    );
    let tracks = store.records().unwrap();
    assert_eq!(tracks.len(), 2);
    let youtube = tracks.iter().find(|r| r.track.source.is_some()).unwrap();
    assert_eq!(youtube.track.title, "김동률 노래");
    assert_eq!(youtube.track.artist, "김동률");
    assert_eq!(youtube.track.album, "");
    assert_ne!(youtube.track.id, "original-youtube");
    crate::deletion::managed_path(&target, &youtube.track).unwrap();
    assert_eq!(
        fs::read(youtube.track.cover.as_ref().unwrap()).unwrap(),
        fs::read(paths.data.join("imports/youtube/VIDEO000001/cover.jpg")).unwrap()
    );
    let scan = library::scan(
        &store.roots().unwrap(),
        &store.records().unwrap(),
        &target.cache,
    );
    store.replace_catalog(&scan.records).unwrap();
    assert_eq!(store.track(&youtube.track.id).unwrap().unwrap().album, "");
    assert_eq!(store.search("", None, 0, 10).unwrap().1, 3);

    store
        .edit_metadata(
            &youtube.track.id,
            Some("Keep destination edit".into()),
            None,
            None,
            None,
        )
        .unwrap();
    let repeated = restore(&target, &mut store, &archive);
    assert_eq!(
        (repeated.added, repeated.radios, repeated.duplicates),
        (0, 0, 3)
    );
    assert_eq!(
        store.track(&youtube.track.id).unwrap().unwrap().title,
        "Keep destination edit"
    );
    drop(store);
    let store = Store::open(&target.database()).unwrap();
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(fs::read(archive).unwrap(), before);
}

#[test]
fn archive_always_includes_local_audio_and_never_reconnects_source_paths() {
    let (_home, paths, _store, local) = fixture();
    let archive = paths.data.join("full.tar.gz");
    assert_eq!(export(&paths, &archive).unwrap().included, 2);
    fs::remove_file(&local).unwrap();
    let (_target, target, mut store) = empty();
    assert_eq!(preview(&target, &archive).unwrap().added, 2);
    assert_eq!(restore(&target, &mut store, &archive).added, 2);
    assert!(
        store
            .roots()
            .unwrap()
            .iter()
            .all(|p| p.starts_with(&target.data))
    );
    let tracks = store.records().unwrap();
    let copied = tracks.iter().find(|r| r.track.source.is_none()).unwrap();
    assert!(copied.track.playback.file().unwrap().is_file());
    assert_ne!(copied.track.playback.file().unwrap(), local);
    let scan = library::scan(&store.roots().unwrap(), &tracks, &target.cache);
    store.replace_catalog(&scan.records).unwrap();
    assert_eq!(
        store.track(&copied.track.id).unwrap().unwrap().album,
        "Local album"
    );
}

#[test]
fn archive_snapshot_reads_wal_without_starting_server_or_creating_missing_database() {
    let (_home, paths, store, local) = fixture();
    assert!(paths.database().with_extension("db-wal").exists());
    let archive = paths.data.join("snapshot.tar.gz");
    export(&paths, &archive).unwrap();
    let home = tempfile::tempdir().unwrap();
    let target = Paths {
        data: home.path().join("absent"),
        runtime: home.path().join("run"),
        cache: home.path().join("cache"),
    };
    assert_eq!(preview(&target, &archive).unwrap().added, 2);
    assert!(!target.data.exists());
    assert!(!target.runtime.exists());
    assert!(local.exists());
    assert_eq!(store.records().unwrap().len(), 2);
}

#[test]
fn archive_rolls_back_database_failure_and_recovers_both_commit_outcomes() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("restore.tar.gz");
    export(&paths, &archive).unwrap();
    let (_home, target, mut store) = empty();
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    publication
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    let connection = rusqlite::Connection::open(target.database()).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_archive BEFORE INSERT ON tracks BEGIN SELECT RAISE(FAIL,'injected archive failure'); END;").unwrap();
    assert!(store.commit_archive(&publication).is_err());
    assert!(store.records().unwrap().is_empty());
    assert!(store.roots().unwrap().is_empty());
    publication.finish(&target, false).unwrap();
    assert!(!target.data.join("imports/youtube/VIDEO000001").exists());
    connection
        .execute_batch("DROP TRIGGER fail_archive")
        .unwrap();

    let mut pending = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    pending
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    drop(pending); // Simulated crash before catalog transaction.
    recover(&target, &store).unwrap();
    assert!(!target.data.join("imports/youtube/VIDEO000001").exists());

    let mut committed = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    committed
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    store.commit_archive(&committed).unwrap();
    drop(committed); // Simulated crash after catalog commit, before cleanup.
    recover(&target, &store).unwrap();
    assert!(
        target
            .data
            .join("imports/youtube/VIDEO000001/audio.m4a")
            .is_file()
    );
    assert_eq!(store.records().unwrap().len(), 2);
    assert_eq!(
        fs::read_dir(target.data.join("archives/.staging"))
            .unwrap()
            .count(),
        0
    );
}

fn rewrite(input: &Path, output: &Path, mutate: impl FnOnce(&mut Manifest), extra: bool) {
    let temp = tempfile::tempdir().unwrap();
    let mut manifest = extract(
        input,
        temp.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    let originals: Vec<_> = manifest.tracks.iter().flat_map(assets).cloned().collect();
    mutate(&mut manifest);
    let mut tar = tar::Builder::new(GzEncoder::new(
        File::create(output).unwrap(),
        Compression::fast(),
    ));
    let json = serde_json::to_vec(&manifest).unwrap();
    append(
        &mut tar,
        "manifest.json",
        json.len() as u64,
        0,
        json.as_slice(),
    )
    .unwrap();
    for a in &originals {
        append(
            &mut tar,
            &a.path,
            a.bytes,
            0,
            File::open(temp.path().join(&a.path)).unwrap(),
        )
        .unwrap();
    }
    if extra {
        let a = &originals[0];
        append(
            &mut tar,
            &a.path,
            a.bytes,
            0,
            File::open(temp.path().join(&a.path)).unwrap(),
        )
        .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
}

#[test]
fn archive_rejects_corruption_versions_duplicate_paths_traversal_and_truncation() {
    let (_home, paths, _store, _) = fixture();
    let archive = paths.data.join("valid.tar.gz");
    export(&paths, &archive).unwrap();
    let output = paths.data.join("bad.tar.gz");
    rewrite(&archive, &output, |m| m.version = 99, false);
    assert!(preview(&paths, &output).is_err());
    rewrite(
        &archive,
        &output,
        |m| m.tracks[0].audio.sha256 = "0".repeat(64),
        false,
    );
    assert!(preview(&paths, &output).is_err());
    rewrite(
        &archive,
        &output,
        |m| m.tracks[0].audio.path = "media/../../outside.m4a".into(),
        false,
    );
    assert!(preview(&paths, &output).is_err());
    rewrite(&archive, &output, |_| (), true);
    assert!(preview(&paths, &output).is_err());
    let bytes = fs::read(&archive).unwrap();
    for length in [bytes.len() / 2, bytes.len() - 4] {
        fs::write(&output, &bytes[..length]).unwrap();
        assert!(preview(&paths, &output).is_err());
    }
    assert_eq!(
        fs::read_dir(paths.data.join("imports/youtube"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn archive_missing_local_audio_fails_instead_of_exporting_a_reference() {
    let (_home, paths, _store, local) = fixture();
    fs::remove_file(local).unwrap();
    let archive = paths.data.join("missing.tar.gz");
    assert!(export(&paths, &archive).is_err());
    assert!(!archive.exists());
}

#[test]
fn archive_recovery_preserves_committed_files_after_unregistering_every_track() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("restore.tar.gz");
    export(&paths, &archive).unwrap();
    let (_home, target, mut store) = empty();
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    publication
        .publish(&target, &stop, &mut Tracker::silent())
        .unwrap();
    store.commit_archive(&publication).unwrap();
    let files: Vec<_> = publication
        .records
        .iter()
        .map(|r| r.record.track.playback.file().unwrap().to_path_buf())
        .collect();
    // A deferred cleanup journal outlives later, legitimate catalog changes.
    store.replace_catalog(&[]).unwrap();
    drop(publication);
    recover(&target, &store).unwrap();
    assert!(files.iter().all(|p| p.is_file()));
    assert!(store.records().unwrap().is_empty());
}

#[test]
fn archive_rejects_links_and_missing_payloads_without_touching_external_files() {
    let (_home, paths, _store, local) = fixture();
    let valid = paths.data.join("valid.tar.gz");
    export(&paths, &valid).unwrap();
    let stage = tempfile::tempdir().unwrap();
    let manifest = extract(
        &valid,
        stage.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    let json = serde_json::to_vec(&manifest).unwrap();
    let original = fs::read(&local).unwrap();
    for entry_type in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
    ] {
        let bad = paths.data.join("link.tar.gz");
        let mut tar = tar::Builder::new(GzEncoder::new(
            File::create(&bad).unwrap(),
            Compression::fast(),
        ));
        append(
            &mut tar,
            "manifest.json",
            json.len() as u64,
            0,
            json.as_slice(),
        )
        .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(entry_type);
        header.set_size(0);
        header.set_mode(0o600);
        header.set_link_name(&local).unwrap();
        header.set_cksum();
        tar.append_data(&mut header, &manifest.tracks[0].audio.path, io::empty())
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        assert!(preview(&paths, &bad).is_err());
        assert_eq!(fs::read(&local).unwrap(), original);
    }
    let bad = paths.data.join("absent.tar.gz");
    let mut tar = tar::Builder::new(GzEncoder::new(
        File::create(&bad).unwrap(),
        Compression::fast(),
    ));
    append(
        &mut tar,
        "manifest.json",
        json.len() as u64,
        0,
        json.as_slice(),
    )
    .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    assert!(preview(&paths, &bad).is_err());
}

#[test]
fn archive_publication_failure_cleans_its_files_and_preserves_existing_destination() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("restore.tar.gz");
    export(&paths, &archive).unwrap();
    let (_home, target, store) = empty();
    let stop = AtomicBool::new(false);
    let mut publication = prepare(
        &target,
        &archive,
        store.archive_catalog().unwrap(),
        &stop,
        &mut Tracker::silent(),
    )
    .unwrap();
    let occupied = publication.records[1]
        .record
        .track
        .playback
        .file()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(&occupied).unwrap();
    fs::write(occupied.join("personal.txt"), b"do not delete").unwrap();
    assert!(
        publication
            .publish(&target, &stop, &mut Tracker::silent())
            .is_err()
    );
    publication.finish(&target, false).unwrap();
    assert_eq!(
        fs::read(occupied.join("personal.txt")).unwrap(),
        b"do not delete"
    );
    assert!(!target.data.join("imports/youtube/VIDEO000001").exists());
    assert!(store.records().unwrap().is_empty());
}

#[test]
fn archive_progress_reports_work_before_publication_and_exact_payload_totals() {
    let (_source, paths, _store, _) = fixture();
    let archive = paths.data.join("progress.tar.gz");
    let mut events = vec![];
    export_with_progress(&paths, &archive, &mut |p| {
        if p.stage == "hashing" || p.stage == "compressing" {
            assert!(!archive.exists());
        }
        events.push(p.clone());
    })
    .unwrap();
    let hashing = events
        .iter()
        .find(|p| p.stage == "hashing" && p.items_done == p.items_total)
        .unwrap();
    assert_eq!(hashing.items_total, 2);
    assert!(hashing.bytes_done > 0);
    assert_eq!(hashing.bytes_total, None);
    let compressed = events
        .iter()
        .find(|p| p.stage == "compressing" && p.items_done == p.items_total)
        .unwrap();
    assert_eq!(compressed.items_total, 2); // Embedded covers need no separate assets.
    assert_eq!(Some(compressed.bytes_done), compressed.bytes_total);
    assert_eq!(events.last().unwrap().stage, "completed");

    let (_target, target, _) = empty();
    let mut restored = vec![];
    preview_with_progress(&target, &archive, &mut |p| restored.push(p.clone())).unwrap();
    let extracted = restored
        .iter()
        .find(|p| p.stage == "extracting" && p.items_done == p.items_total)
        .unwrap();
    assert_eq!(extracted.bytes_done, compressed.bytes_done);
    assert_eq!(extracted.bytes_total, compressed.bytes_total);
    assert!(restored.iter().any(|p| p.stage == "validating"));
    assert!(restored.iter().any(|p| p.stage == "planning"));
}

#[test]
fn archive_names_are_readable_when_opened_with_a_standard_tar_reader() {
    let (_home, paths, _store, _) = fixture();
    let path = paths.data.join("readable.tar.gz");
    export(&paths, &path).unwrap();
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(&path).unwrap()));
    let names: Vec<_> = tar
        .entries()
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        names.contains(&"김동률 - 김동률 노래.m4a".to_owned()),
        "{names:?}"
    );
    assert!(names.iter().all(|name| !name.contains('/')));
    assert_eq!(names.len(), 3); // Manifest and two audio files; no cover sidecars.
    let (_target, target, mut store) = empty();
    assert_eq!(restore(&target, &mut store, &path).added, 2);
    let extracted = tempfile::tempdir().unwrap();
    tar::Archive::new(GzDecoder::new(File::open(&path).unwrap()))
        .unpack(extracted.path())
        .unwrap();
    let audio = extracted.path().join("김동률 - 김동률 노래.m4a");
    let tag = mp4ameta::Tag::read_from_path(&audio).unwrap();
    assert_eq!(tag.title(), Some("김동률 노래"));
    assert_eq!(tag.artist(), Some("김동률"));
    assert_eq!(tag.album(), None);
    assert_eq!(
        tag.artwork().unwrap().data,
        fs::read(paths.data.join("imports/youtube/VIDEO000001/cover.jpg")).unwrap()
    );
    let track =
        library::read_track(&audio, "extracted".into(), &extracted.path().join("cache")).unwrap();
    assert!(track.cover.unwrap().is_file());
}

#[test]
fn readable_names_keep_unicode_and_disambiguate_sanitized_or_duplicate_titles() {
    let mut used = HashSet::new();
    let mut track = Track {
        id: "unused".into(),
        playback: PlaybackSource::File {
            path: "/unused.m4a".into(),
        },
        title: "감사".into(),
        artist: "김동률".into(),
        album: String::new(),
        track_number: 0,
        duration_ms: None,
        cover: None,
        video: false,
        source: None,
    };
    assert_eq!(readable_name(&track, &mut used), "김동률 - 감사");
    assert_eq!(readable_name(&track, &mut used), "김동률 - 감사 (2)");
    track.artist.clear();
    track.title = "A/B: C?".into();
    assert_eq!(readable_name(&track, &mut used), "A B C");
    track.title = "a b c".into();
    assert_eq!(readable_name(&track, &mut used), "a b c (2)");
    track.title = "아주 긴 노래 제목 ".repeat(100);
    let long = readable_name(&track, &mut used);
    assert!(long.len() <= 80);
    assert!(long.starts_with("아주 긴 노래 제목"));
    let mut tar = tar::Builder::new(Vec::new());
    append(&mut tar, &format!("{long}.mkv"), 1, 0, &[0u8][..]).unwrap();
}

fn mp4_payload(data: &[u8]) -> Vec<u8> {
    let mut offset = 0;
    let mut payload = vec![];
    while offset + 8 <= data.len() {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap());
        let (size, header) = match size {
            0 => (data.len() - offset, 8),
            1 => (
                u64::from_be_bytes(data[offset + 8..offset + 16].try_into().unwrap()) as usize,
                16,
            ),
            n => (n as usize, 8),
        };
        assert!(size >= header && offset + size <= data.len());
        if &data[offset + 4..offset + 8] == b"mdat" {
            payload.extend_from_slice(&data[offset + header..offset + size]);
        }
        offset += size;
    }
    assert!(!payload.is_empty());
    payload
}

#[test]
fn archive_embeds_tags_without_changing_originals_or_aac_payload_and_deduplicates_originals() {
    let (_home, paths, mut store, local) = fixture();
    let records = store.records().unwrap();
    let originals: Vec<_> = records
        .iter()
        .map(|r| {
            let path = r.track.playback.file().unwrap();
            (path.to_owned(), fs::read(path).unwrap())
        })
        .collect();
    let archive = paths.data.join("tags.tar.gz");
    export(&paths, &archive).unwrap();
    for (path, before) in &originals {
        assert_eq!(&fs::read(path).unwrap(), before);
    }
    let unpacked = tempfile::tempdir().unwrap();
    let manifest = extract(
        &archive,
        unpacked.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    for entry in &manifest.tracks {
        let bytes = fs::read(unpacked.path().join(&entry.audio.path)).unwrap();
        assert_eq!(mp4_payload(&bytes), mp4_payload(&originals[0].1));
        assert_ne!(entry.audio.sha256, entry.original_sha256);
    }
    let report = restore(&paths, &mut store, &archive);
    assert_eq!((report.added, report.duplicates), (0, 3));
    assert_eq!(store.records().unwrap().len(), 2);
    assert!(local.exists());
}

#[test]
fn archive_tag_and_missing_tool_failures_never_publish_or_modify_originals() {
    let (_home, paths, _store, local) = fixture();
    let archive = paths.data.join("failed.tar.gz");
    fs::write(&local, b"damaged audio").unwrap();
    let error = export(&paths, &archive).unwrap_err();
    assert!(
        format!("{error:#}").contains("Cannot embed tags"),
        "{error:#}"
    );
    assert!(!archive.exists());
    assert_eq!(fs::read(&local).unwrap(), b"damaged audio");

    fs::write(
        paths.data.join("imports/youtube/VIDEO000001/video.mkv"),
        b"video",
    )
    .unwrap();
    platform::atomic_json(&paths.data.join("imports.json"), &serde_json::json!({
        "youtube": { "ffmpeg": paths.data.join("missing-ffmpeg"), "ffprobe": paths.data.join("missing-ffprobe") }
    })).unwrap();
    let error = export(&paths, &archive).unwrap_err();
    assert!(
        format!("{error:#}").contains("require installed FFmpeg"),
        "{error:#}"
    );
    assert!(!archive.exists());
}

#[test]
fn archive_headers_and_normal_extraction_preserve_source_modification_times() {
    let (_home, paths, _store, local) = fixture();
    let downloaded = paths.data.join("imports/youtube/VIDEO000001/audio.m4a");
    let times = [(downloaded, 1_650_000_123), (local, 1_660_000_321)];
    for (file, seconds) in &times {
        File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(*seconds)),
            )
            .unwrap();
    }
    let started = tar_time(SystemTime::now()).unwrap();
    let archive = paths.data.join("times.tar.gz");
    export(&paths, &archive).unwrap();
    let finished = tar_time(SystemTime::now()).unwrap();
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(&archive).unwrap()));
    let headers: BTreeMap<_, _> = tar
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.path().unwrap().into_owned(),
                entry.header().mtime().unwrap(),
            )
        })
        .collect();
    let manifest_time = headers[Path::new("manifest.json")];
    assert!((started..=finished).contains(&manifest_time));
    let mut media_times: Vec<_> = headers
        .iter()
        .filter(|(p, _)| **p != Path::new("manifest.json"))
        .map(|(_, t)| *t)
        .collect();
    media_times.sort();
    assert_eq!(media_times, vec![1_650_000_123, 1_660_000_321]);
    let extracted = tempfile::tempdir().unwrap();
    tar::Archive::new(GzDecoder::new(File::open(&archive).unwrap()))
        .unpack(extracted.path())
        .unwrap();
    for (name, expected) in headers {
        assert_eq!(
            tar_time(
                extracted
                    .path()
                    .join(name)
                    .metadata()
                    .unwrap()
                    .modified()
                    .unwrap()
            )
            .unwrap(),
            expected
        );
    }
    for (file, expected) in times {
        assert_eq!(
            tar_time(file.metadata().unwrap().modified().unwrap()).unwrap(),
            expected
        );
    }
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let binary = crate::subprocess::executable(None, "ffmpeg").unwrap();
    let output = std::process::Command::new(binary)
        .args(["-nostdin", "-v", "error"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn packet_hashes(path: &Path, stream: &str) -> Vec<String> {
    let output =
        std::process::Command::new(crate::subprocess::executable(None, "ffprobe").unwrap())
            .args([
                "-v",
                "error",
                "-select_streams",
                stream,
                "-show_packets",
                "-show_data_hash",
                "sha256",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    value["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["data_hash"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
#[ignore = "Needs installed FFmpeg/FFprobe; uses only generated media"]
fn archive_real_video_exports_sound_and_restores_silent_sidecar_without_reencoding() {
    let (_home, paths, store, _) = fixture();
    let folder = paths.data.join("imports/youtube/VIDEO000001");
    let silent = folder.join("video.mkv");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=blue:s=32x32:r=10:d=1",
        "-an",
        "-c:v",
        "ffv1",
        silent.to_str().unwrap(),
    ]);
    let video_time = 1_640_000_789;
    File::options()
        .write(true)
        .open(&silent)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(video_time)),
        )
        .unwrap();
    let original_audio = fs::read(folder.join("audio.m4a")).unwrap();
    let original_video = fs::read(&silent).unwrap();
    let archive = paths.data.join("video.tar.gz");
    let report = export(&paths, &archive).unwrap();
    assert_eq!((report.included, report.videos), (2, 1));
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(&archive).unwrap()));
    let video_header_time = tar
        .entries()
        .unwrap()
        .find_map(|entry| {
            let entry = entry.unwrap();
            entry
                .path()
                .unwrap()
                .extension()
                .is_some_and(|ext| ext == "mkv")
                .then(|| entry.header().mtime().unwrap())
        })
        .unwrap();
    assert_eq!(video_header_time, video_time);
    assert_eq!(fs::read(folder.join("audio.m4a")).unwrap(), original_audio);
    assert_eq!(fs::read(&silent).unwrap(), original_video);
    let unpacked = tempfile::tempdir().unwrap();
    let manifest = extract(
        &archive,
        unpacked.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    let entry = manifest.tracks.iter().find(|e| e.video.is_some()).unwrap();
    let video = unpacked.path().join(&entry.video.as_ref().unwrap().path);
    assert_eq!(video.file_name().unwrap(), "김동률 - 김동률 노래.mkv");
    let tools = media::Tools::load(&paths).unwrap();
    tools.probe(&video, true, &AtomicBool::new(false)).unwrap();
    assert_eq!(packet_hashes(&video, "v:0"), packet_hashes(&silent, "v:0"));
    assert_eq!(
        packet_hashes(&video, "a:0"),
        packet_hashes(&folder.join("audio.m4a"), "a:0")
    );
    let (_target, target, mut target_store) = empty();
    assert_eq!(preview(&target, &archive).unwrap().added, 2);
    restore(&target, &mut target_store, &archive);
    let restored = target_store
        .records()
        .unwrap()
        .into_iter()
        .find(|r| r.track.source.is_some())
        .unwrap();
    let restored_dir = crate::deletion::managed_path(&target, &restored.track).unwrap();
    tools
        .probe(
            &restored_dir.join("video.mkv"),
            false,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        packet_hashes(&restored_dir.join("video.mkv"), "v:0"),
        packet_hashes(&silent, "v:0")
    );
    assert!(restored.track.cover.unwrap().is_file());
    assert_eq!(store.records().unwrap().len(), 2);
}

#[test]
#[ignore = "Needs installed FFmpeg/FFprobe; uses only generated media"]
fn archive_real_audio_formats_embed_art_and_tags_and_preserve_decoded_samples() {
    let (_home, paths, mut store) = empty();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stereo.wav");
    let music = paths.data.join("music");
    fs::create_dir(&music).unwrap();
    image::RgbImage::from_pixel(8, 8, image::Rgb([10, 50, 100]))
        .save(music.join("cover.png"))
        .unwrap();
    let mut records = vec![];
    for (index, (ext, codec)) in [
        ("wav", "pcm_s16le"),
        ("m4a", "aac"),
        ("mp3", "libmp3lame"),
        ("flac", "flac"),
        ("ogg", "libvorbis"),
        ("aac", "aac"),
    ]
    .into_iter()
    .enumerate()
    {
        let file = music.join(format!("track.{ext}"));
        ffmpeg(&[
            "-i",
            source.to_str().unwrap(),
            "-c:a",
            codec,
            file.to_str().unwrap(),
        ]);
        let mut record = record(&file, &format!("format-{index}"), &paths);
        record.track.title = format!("한글 {ext}");
        record.track.artist = "음악가".into();
        record.track.album = String::new();
        records.push(record);
    }
    store.replace_catalog(&records).unwrap();
    let original_bytes: Vec<_> = records
        .iter()
        .map(|r| fs::read(r.track.playback.file().unwrap()).unwrap())
        .collect();
    let archive = paths.data.join("formats.tar.gz");
    export(&paths, &archive).unwrap();
    let unpacked = tempfile::tempdir().unwrap();
    let manifest = extract(
        &archive,
        unpacked.path(),
        &AtomicBool::new(false),
        &mut Tracker::silent(),
    )
    .unwrap();
    for (record, original) in records.iter().zip(original_bytes) {
        let source = record.track.playback.file().unwrap();
        assert_eq!(fs::read(source).unwrap(), original);
        let entry = manifest
            .tracks
            .iter()
            .find(|e| e.track.title == record.track.title)
            .unwrap();
        let file = unpacked.path().join(&entry.audio.path);
        let read =
            library::read_track(&file, "probe".into(), &unpacked.path().join("cache")).unwrap();
        assert_eq!(read.title, record.track.title);
        assert_eq!(read.artist, record.track.artist);
        assert_eq!(read.album, "");
        assert!(media::embedded_cover(&file).unwrap().is_some());
        let before = ffmpeg(&[
            "-i",
            source.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "pipe:1",
        ]);
        let after = ffmpeg(&[
            "-i",
            file.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "pipe:1",
        ]);
        assert_eq!(before, after, "{}", file.display());
        let native_before: Vec<_> = crate::audio::decode_file(source).unwrap().collect();
        let native_after: Vec<_> = crate::audio::decode_file(&file).unwrap().collect();
        assert_eq!(
            native_before,
            native_after,
            "vtamp decoder: {}",
            file.display()
        );
    }
}
