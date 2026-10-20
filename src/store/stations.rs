// The owner wires these in a later task; until then only the tests use them.
#![allow(dead_code)]
use super::{
    Store,
    playlists::{invalid, next_revision},
};
use crate::protocol::{MAX_STATION_BATCH, MAX_STATIONS, Station, StreamSpec};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::fmt;

/// The caller's stations revision is not the current one.
#[derive(Debug)]
pub(crate) struct StaleStationsRevision {
    expected: u64,
    current: u64,
}
impl fmt::Display for StaleStationsRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "stations revision {} is stale; current {}",
            self.expected, self.current
        )
    }
}
impl std::error::Error for StaleStationsRevision {}

#[derive(Debug)]
pub(crate) enum StationEdit {
    Add(Vec<StreamSpec>),
    Update { id: u64, spec: StreamSpec },
    Remove { id: u64 },
}

/// The new revision and the rows an edit added or updated; empty for a remove.
#[derive(Debug)]
pub(crate) struct StationChange {
    pub revision: u64,
    pub stations: Vec<Station>,
}

pub(super) fn migrate(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "CREATE TABLE stations_state (revision INTEGER NOT NULL);
        INSERT INTO stations_state VALUES(0);
        CREATE TABLE stations (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL UNIQUE,
            sort_key TEXT NOT NULL
        );
        CREATE INDEX stations_order ON stations(sort_key,id);",
    )?;
    Ok(())
}

fn revision(connection: &Connection) -> Result<u64> {
    Ok(connection.query_row("SELECT revision FROM stations_state", [], |row| row.get(0))?)
}
fn station_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Station> {
    Ok(Station {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
    })
}
fn insert(connection: &Connection, spec: &StreamSpec) -> Result<Option<Station>> {
    Ok(connection
        .query_row(
            "INSERT INTO stations(name,url,sort_key) VALUES(?1,?2,?3)
             ON CONFLICT(url) DO NOTHING RETURNING id,name,url",
            params![spec.title, spec.url, spec.title.to_lowercase()],
            station_row,
        )
        .optional()?)
}

impl Store {
    pub fn stations_revision(&self) -> Result<u64> {
        revision(&self.connection)
    }

    /// The revision and every station, sorted by name without regard to case.
    pub fn stations(&self) -> Result<(u64, Vec<Station>)> {
        let transaction = self.connection.unchecked_transaction()?;
        let current = revision(&transaction)?;
        let stations = transaction
            .prepare("SELECT id,name,url FROM stations ORDER BY sort_key,id")?
            .query_map([], station_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok((current, stations))
    }

    pub fn edit_stations(&mut self, expected: u64, edit: StationEdit) -> Result<StationChange> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = revision(&transaction)?;
        if expected != current {
            return Err(StaleStationsRevision { expected, current }.into());
        }
        let stations = match edit {
            StationEdit::Add(specs) => {
                if specs.is_empty() || specs.len() > MAX_STATION_BATCH {
                    return Err(invalid(format!(
                        "adding stations requires 1..={MAX_STATION_BATCH} stations"
                    )));
                }
                let mut added = Vec::new();
                for spec in &specs {
                    added.extend(insert(&transaction, spec)?);
                }
                let count: usize =
                    transaction.query_row("SELECT COUNT(*) FROM stations", [], |row| row.get(0))?;
                if count > MAX_STATIONS {
                    return Err(invalid(format!("at most {MAX_STATIONS} stations fit")));
                }
                added
            }
            StationEdit::Update { id, spec } => {
                let taken: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stations WHERE url=?1 AND id<>?2)",
                    params![spec.url, id],
                    |row| row.get(0),
                )?;
                if taken {
                    return Err(invalid("another station already uses that URL"));
                }
                let station = transaction
                    .query_row(
                        "UPDATE stations SET name=?2,url=?3,sort_key=?4 WHERE id=?1
                         RETURNING id,name,url",
                        params![id, spec.title, spec.url, spec.title.to_lowercase()],
                        station_row,
                    )
                    .optional()?
                    .ok_or_else(|| invalid("station not found"))?;
                vec![station]
            }
            StationEdit::Remove { id } => {
                if transaction.execute("DELETE FROM stations WHERE id=?1", [id])? == 0 {
                    return Err(invalid("station not found"));
                }
                Vec::new()
            }
        };
        let revision = next_revision(current)?;
        transaction.execute("UPDATE stations_state SET revision=?1", [revision])?;
        transaction.commit()?;
        Ok(StationChange { revision, stations })
    }
}
