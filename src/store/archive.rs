use super::*;
use crate::archive::{Catalog, SavedMetadata};
use rusqlite::OpenFlags;
use std::collections::BTreeMap;

impl Store {
    /// Read a consistent WAL-aware snapshot without migrating or creating the source.
    pub(crate) fn archive_snapshot(path: &Path) -> Result<Catalog> {
        if !path.exists() {
            return Ok(Catalog::default());
        }
        let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut snapshot = Connection::open_in_memory()?;
        rusqlite::backup::Backup::new(&source, &mut snapshot)?.run_to_completion(
            128,
            std::time::Duration::from_millis(5),
            None,
        )?;
        let version: u32 = snapshot.pragma_query_value(None, "user_version", |r| r.get(0))?;
        anyhow::ensure!(
            matches!(version, 6..=8),
            "Library archive requires database version 6 to 8"
        );
        Self { db: snapshot }.archive_catalog()
    }

    pub(crate) fn archive_catalog(&self) -> Result<Catalog> {
        let mut metadata = BTreeMap::new();
        let mut statement = self.db.prepare(
            "SELECT id,metadata,title_override,artist_override,album_override FROM track_metadata",
        )?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (id, automatic, title, artist, album) = row?;
            metadata.insert(
                id,
                SavedMetadata {
                    automatic: serde_json::from_str(&automatic)?,
                    title,
                    artist,
                    album,
                },
            );
        }
        let streams = self
            .db
            .prepare("SELECT json FROM streams ORDER BY path")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| {
                let record: Record = serde_json::from_str(&row?)?;
                let crate::model::PlaybackSource::Stream { url } = record.track.playback else {
                    bail!("Invalid stream record");
                };
                Ok(crate::streams::Entry {
                    url,
                    name: record.track.title,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Catalog {
            records: self.records()?,
            metadata,
            streams,
        })
    }

    pub(crate) fn commit_archive(
        &mut self,
        publication: &crate::archive::Publication,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        for item in &publication.records {
            let record = &item.record;
            let saved = &item.metadata;
            let manifest = record
                .track
                .source
                .as_ref()
                .map(|source| crate::imports::Manifest {
                    track_id: record.track.id.clone(),
                    source: source.clone(),
                    metadata: saved.automatic.clone(),
                    title_override: saved.title.clone(),
                    artist_override: saved.artist.clone(),
                });
            tx.execute(
                "INSERT INTO track_metadata(id,video_id,manifest,metadata,title_override,artist_override,album_override) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    record.track.id,
                    record.track.source.as_ref().map(|s| &s.video_id),
                    manifest.map(|m| serde_json::to_string(&m)).transpose()?,
                    serde_json::to_string(&saved.automatic)?,
                    saved.title, saved.artist, saved.album,
                ],
            )?;
            imports::write_record(&tx, record)?;
        }
        for root in &publication.roots {
            tx.execute(
                "INSERT OR IGNORE INTO roots(path) VALUES(?1)",
                [root.to_string_lossy().as_ref()],
            )?;
        }
        for entry in &publication.streams {
            let track = entry.track();
            let record = Record {
                track: track.clone(),
                modified: 0,
                bytes: 0,
            };
            tx.execute(
                "INSERT INTO streams(id,path,search,json,title_search,artist_search,album_search,kind) VALUES(?1,?2,?3,?4,?3,'','','radio') ON CONFLICT(path) DO NOTHING",
                params![track.id, entry.url, normalized(&entry.name), serde_json::to_string(&record)?],
            )?;
        }
        // '/' cannot appear in client queue-edit request IDs. This internal
        // receipt distinguishes commit from rollback after catalog changes.
        tx.execute(
            "INSERT INTO requests(id,payload,reply,expires_ms) VALUES(?1,'library_archive_commit','{}',?2)",
            params![publication.receipt(), i64::MAX],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn archive_committed(&self, id: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM requests WHERE id=?1 AND payload='library_archive_commit')",
            [id], |r| r.get(0),
        )?)
    }

    pub(crate) fn clear_archive_receipt(&self, id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM requests WHERE id=?1 AND payload='library_archive_commit'",
            [id],
        )?;
        Ok(())
    }

    pub(crate) fn clear_archive_receipts(&self) -> Result<()> {
        self.db.execute(
            "DELETE FROM requests WHERE id LIKE '/archive/%' AND payload='library_archive_commit'",
            [],
        )?;
        Ok(())
    }
}
