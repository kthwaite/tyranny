//! Resolves run selections on a blocking worker with its own read-only
//! connection, so a large selection never holds up the playback owner. The
//! owner then only rechecks revisions and commits (`Actor::resolved`).
use super::{CommandResult, Failure, source_name, source_path};
use crate::{
    listing::{recent_cutoff, unix_now},
    protocol::Command,
    store::resolve_selection,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// A selection's tracks with their canonical source paths, in selection
/// order, or why the selection cannot be enqueued.
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

    /// Resolves the selection of `command` (`EnqueueTracks` or
    /// `PlaylistAddSelection`) on a blocking worker. Returns the command with
    /// its resolution.
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
        let selection = match command {
            Command::EnqueueTracks {
                selection, source, ..
            } => {
                // Needs no SQL, so a bad name skips the resolution.
                source_name(source)?;
                selection
            }
            Command::PlaylistAddSelection { selection, .. } => selection,
            _ => unreachable!("only run selections are resolved before the owner"),
        };
        let mut connection = self.connection().map_err(Failure::persistence)?;
        let mut sources = resolve_selection(&mut connection, selection, recent_cutoff(unix_now()))
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
