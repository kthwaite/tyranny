//! Resolves run selections and enqueued playlists on a blocking worker with
//! its own read-only connection, so a large selection never holds up the
//! playback owner. The owner then only rechecks revisions and commits
//! (`Actor::resolved`).
use super::{CommandResult, Failure, source_name, source_path};
use crate::{
    listing::{recent_cutoff, unix_now},
    protocol::Command,
    store::{playlist_sources, resolve_selection},
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// A selection's (or playlist's) tracks with their canonical source paths, in
/// order, or why they cannot be enqueued.
pub(super) type Resolution = CommandResult<Vec<(u64, PathBuf)>>;

#[derive(Clone)]
pub(super) struct Resolver {
    database: PathBuf,
    /// The capture output, which cannot also be a queued source.
    capture: Option<PathBuf>,
    /// Test seam: runs on the worker after a selection resolves, before the
    /// owner commits it.
    #[cfg(test)]
    pub(super) resolved: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl Resolver {
    pub(super) fn new(database: PathBuf, capture: Option<PathBuf>) -> Self {
        Self {
            database,
            capture,
            #[cfg(test)]
            resolved: None,
        }
    }

    /// Resolves the tracks `command` (`EnqueueTracks`, `PlaylistAddSelection`
    /// or `EnqueuePlaylist`) names on a blocking worker. Returns the command
    /// with its resolution.
    pub(super) async fn resolve(
        self: Arc<Self>,
        command: Command,
    ) -> Result<(Command, Resolution)> {
        tokio::task::spawn_blocking(move || {
            let resolution = self.sources(&command);
            #[cfg(test)]
            if let Some(resolved) = &self.resolved {
                resolved();
            }
            (command, resolution)
        })
        .await
        .context("selection resolver failed")
    }

    fn sources(&self, command: &Command) -> Resolution {
        if let Command::EnqueueTracks { source, .. } = command {
            // Needs no SQL, so a bad name skips the resolution.
            source_name(source)?;
        }
        let mut connection = self.connection().map_err(Failure::persistence)?;
        let mut sources = match command {
            Command::EnqueueTracks { selection, .. }
            | Command::PlaylistAddSelection { selection, .. } => {
                resolve_selection(&mut connection, selection, recent_cutoff(unix_now()))
            }
            Command::EnqueuePlaylist {
                playlist_id,
                playlist_revision,
                library_revision,
                ..
            } => playlist_sources(
                &mut connection,
                *playlist_id,
                *playlist_revision,
                *library_revision,
            ),
            _ => unreachable!("only run selections are resolved before the owner"),
        }
        .map_err(Failure::playlist)?;
        for (_, path) in &mut sources {
            *path = source_path(std::mem::take(path), self.capture.as_deref())?;
        }
        Ok(sources)
    }

    /// Opened like the artwork workers' library connections.
    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("opening the library for selection resolution")?;
        connection.busy_timeout(Duration::from_millis(100))?;
        Ok(connection)
    }
}
