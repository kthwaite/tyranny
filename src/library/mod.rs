//! Durable local-library indexing runs entirely off the playback owner. One
//! coordinator owns SQL and bounded worklists; one scanner owns filesystem and
//! native metadata IO. Root completion watermarks make reconciliation atomic.
mod db;
mod query;
#[cfg(test)]
mod tests;

use crate::{
    library_types::*,
    scanner::{ScanEvent, Scanner},
};
use anyhow::{Context, Result, anyhow, ensure};
pub(crate) use db::{migrate, migrate_v5, resolve_track};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc as startup,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
struct Work {
    command: LibraryCommand,
    reply: oneshot::Sender<std::result::Result<LibraryReply, LibraryError>>,
}

pub struct Library {
    handle: LibraryHandle,
    shared: Arc<RwLock<LibraryState>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}
#[derive(Clone)]
pub struct LibraryHandle {
    sender: mpsc::Sender<Work>,
    wake: thread::Thread,
}

fn error(kind: LibraryErrorKind, message: impl Into<String>) -> LibraryError {
    LibraryError {
        kind,
        message: db::bounded_error(message.into()),
    }
}
fn classify(failure: anyhow::Error) -> LibraryError {
    if failure.downcast_ref::<StaleLibraryRevision>().is_some() {
        error(LibraryErrorKind::StaleRevision, failure.to_string())
    } else if failure.is::<crate::filter::InvalidFilter>() {
        error(LibraryErrorKind::InvalidFilter, failure.to_string())
    } else if let Some(failure) = failure.downcast_ref::<LibraryError>() {
        error(failure.kind, &failure.message)
    } else {
        error(LibraryErrorKind::Storage, format!("{failure:#}"))
    }
}
fn publish(shared: &RwLock<LibraryState>, state: LibraryState) {
    *shared.write().unwrap_or_else(|poison| poison.into_inner()) = state;
}

impl Library {
    pub fn start(database_path: PathBuf) -> Result<Self> {
        let shared = Arc::new(RwLock::new(LibraryState {
            revision: 1,
            roots: 0,
            tracks: 0,
            scan: ScanStatus::default(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel(32);
        let (ready_tx, ready_rx) = startup::sync_channel(1);
        let worker_shared = Arc::clone(&shared);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("library-index".into())
            .spawn(move || {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                        match Coordinator::open(database_path, Arc::clone(&worker_shared)) {
                            Ok(coordinator) => {
                                if ready_tx.send(Ok(())).is_err() {
                                    return Ok(());
                                }
                                coordinator.run(receiver, worker_stop)
                            }
                            Err(failure) => {
                                let _ = ready_tx.send(Err(format!("{failure:#}")));
                                Err(failure)
                            }
                        }
                    }))
                    .unwrap_or_else(|_| {
                        Err(anyhow!(
                            "library worker panicked; unfinished scan is not reconciled"
                        ))
                    });
                if let Err(failure) = &outcome {
                    let mut state = worker_shared
                        .write()
                        .unwrap_or_else(|poison| poison.into_inner());
                    state.scan.phase = ScanPhase::Failed;
                    state.scan.last_error = Some(db::bounded_error(format!("{failure:#}")));
                }
                outcome
            })
            .context("starting library worker")?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                handle: LibraryHandle {
                    sender,
                    wake: worker.thread().clone(),
                },
                shared,
                stop,
                worker: Some(worker),
            }),
            outcome => {
                stop.store(true, Ordering::Release);
                worker.thread().unpark();
                let _ = worker.join();
                Err(anyhow!("library initialization failed: {outcome:?}"))
            }
        }
    }
    pub fn handle(&self) -> LibraryHandle {
        self.handle.clone()
    }
    /// A bounded in-memory snapshot: no SQL or filesystem access on the caller.
    pub fn state(&self) -> LibraryState {
        self.shared
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
    pub fn shutdown(mut self) -> Result<()> {
        self.stop_and_join()
    }
    fn stop_and_join(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            worker
                .join()
                .map_err(|_| anyhow!("library worker panicked"))??;
        }
        Ok(())
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}
impl LibraryHandle {
    pub async fn request(
        &self,
        command: LibraryCommand,
    ) -> std::result::Result<LibraryReply, LibraryError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Work { command, reply })
            .map_err(|failure| match failure {
                mpsc::error::TrySendError::Full(_) => {
                    error(LibraryErrorKind::Busy, "library request queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    error(LibraryErrorKind::Storage, "library worker is unavailable")
                }
            })?;
        self.wake.unpark();
        receiver.await.map_err(|_| {
            error(
                LibraryErrorKind::Storage,
                "library worker closed before command result; do not replay automatically",
            )
        })?
    }
}

struct Active {
    scanner: Option<Scanner>,
    serial: u64,
    status: ScanStatus,
    enumerated: bool,
    complete: bool,
    in_flight: usize,
}
struct Coordinator {
    connection: Connection,
    active: Option<Active>,
    shared: Arc<RwLock<LibraryState>>,
    published: Instant,
    dirty: bool,
    cleanup_pending: bool,
    /// Roots still to rescan after a schema upgrade; `pending_upgrade` stays set
    /// in the database until this drains and no scan is active.
    pending_rescans: VecDeque<u64>,
    upgrading: bool,
}
impl Coordinator {
    fn open(path: PathBuf, shared: Arc<RwLock<LibraryState>>) -> Result<Self> {
        // Store has already migrated this exact database; do not silently create
        // an independent database if the supplied path has disappeared.
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        publish(&shared, db::recover(&mut connection)?);
        let upgrading: bool = connection.query_row(
            "SELECT pending_upgrade FROM library_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let pending_rescans = if upgrading {
            connection
                .prepare("SELECT id FROM library_roots ORDER BY id")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<VecDeque<u64>>>()?
        } else {
            VecDeque::new()
        };
        Ok(Self {
            connection,
            active: None,
            shared,
            published: Instant::now(),
            dirty: false,
            cleanup_pending: true,
            pending_rescans,
            upgrading,
        })
    }
    fn run(mut self, mut receiver: mpsc::Receiver<Work>, stop: Arc<AtomicBool>) -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            let mut worked = false;
            match receiver.try_recv() {
                Ok(work) => {
                    worked = true;
                    let response = self.command(work.command).map_err(classify);
                    let _ = work.reply.send(response);
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            if self.active.is_some() {
                self.scan_tick()?;
            }
            if self.upgrading && self.active.is_none() {
                self.upgrade_step()?;
            }
            if self.cleanup_pending {
                let removed = db::discard_old_work(
                    &self.connection,
                    self.active.as_ref().map(|scan| scan.serial),
                )?;
                self.cleanup_pending = removed == 128;
            }
            self.publish(false)?;
            if !worked {
                thread::park_timeout(Duration::from_millis(5));
            }
        }
        if let Some(mut active) = self.active.take() {
            let stopped = active.scanner.take().map(Scanner::shutdown).transpose();
            active.status.phase = ScanPhase::Interrupted;
            active.status.last_error = Some("scan interrupted by shutdown; rescan manually".into());
            self.persist(&active.status)?;
            self.publish(true)?;
            stopped?;
        }
        Ok(())
    }
    /// Runs while idle. Starts the next queued rescan; a root that fails to start
    /// or scan is not retried. Clears the database flag once the queue drains.
    fn upgrade_step(&mut self) -> Result<()> {
        match self.pending_rescans.pop_front() {
            Some(root_id) => {
                let _ = self.rescan(root_id);
            }
            None => {
                self.connection.execute(
                    "UPDATE library_state SET pending_upgrade=0 WHERE singleton=1",
                    [],
                )?;
                self.upgrading = false;
            }
        }
        Ok(())
    }
    fn publish(&mut self, force: bool) -> Result<()> {
        if self.dirty && (force || self.published.elapsed() >= PUBLISH_INTERVAL) {
            publish(&self.shared, db::state(&self.connection)?);
            self.published = Instant::now();
            self.dirty = false;
        }
        Ok(())
    }
    fn persist(&mut self, status: &ScanStatus) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        db::save_status(&transaction, status)?;
        transaction.commit()?;
        self.dirty = true;
        Ok(())
    }
    fn command(&mut self, command: LibraryCommand) -> Result<LibraryReply> {
        match command {
            LibraryCommand::AddRoot { path } => self.add_root(path),
            LibraryCommand::Rescan { root_id } => self.rescan(root_id),
            command => query::execute(&mut self.connection, command),
        }
    }
    fn idle(&self) -> Result<()> {
        if self.active.is_some() {
            return Err(error(
                LibraryErrorKind::Busy,
                "a library scan is already in progress",
            )
            .into());
        }
        Ok(())
    }
    fn add_root(&mut self, path: String) -> Result<LibraryReply> {
        self.idle()?;
        let supplied = Path::new(&path);
        if !supplied.is_absolute() {
            return Err(error(
                LibraryErrorKind::Invalid,
                "library root must be an absolute directory",
            )
            .into());
        }
        let canonical = supplied.canonicalize().map_err(|failure| {
            error(
                LibraryErrorKind::Invalid,
                format!("cannot open library root: {failure}"),
            )
        })?;
        if !canonical.is_dir() {
            return Err(error(LibraryErrorKind::Invalid, "library root is not a directory").into());
        }
        let path = canonical
            .to_str()
            .ok_or_else(|| error(LibraryErrorKind::Invalid, "library root path must be UTF-8"))?
            .to_owned();
        {
            let mut statement = self.connection.prepare("SELECT path FROM library_roots")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let registered: String = row.get(0)?;
                if canonical.starts_with(&registered)
                    || Path::new(&registered).starts_with(&canonical)
                {
                    return Err(error(
                        LibraryErrorKind::Invalid,
                        "library root overlaps a registered root",
                    )
                    .into());
                }
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO library_roots(path,search) VALUES(?1,?2)",
            params![path, path.to_lowercase()],
        )?;
        let root_id = transaction.last_insert_rowid() as u64;
        transaction.execute(
            "UPDATE library_state SET root_count=root_count+1 WHERE singleton=1",
            [],
        )?;
        let (serial, status) = db::begin_scan(&transaction, root_id)?;
        transaction.commit()?;
        self.launch(canonical, serial, status.clone())?;
        Ok(LibraryReply::RootAdded(Root {
            id: root_id,
            path,
            last_scan: Some(status),
        }))
    }
    fn rescan(&mut self, root_id: u64) -> Result<LibraryReply> {
        self.idle()?;
        if root_id == 0 || root_id > i64::MAX as u64 {
            return Err(error(LibraryErrorKind::Invalid, "unknown library root").into());
        }
        let path: String = self
            .connection
            .query_row(
                "SELECT path FROM library_roots WHERE id=?1",
                [root_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| error(LibraryErrorKind::Invalid, "unknown library root"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (serial, status) = db::begin_scan(&transaction, root_id)?;
        transaction.commit()?;
        self.pending_rescans.retain(|queued| *queued != root_id);
        self.launch(path.into(), serial, status)?;
        Ok(LibraryReply::ScanAccepted { root_id })
    }
    fn launch(&mut self, root: PathBuf, serial: u64, mut status: ScanStatus) -> Result<()> {
        self.dirty = true;
        self.cleanup_pending = true;
        match Scanner::start(root) {
            Ok(scanner) => {
                self.active = Some(Active {
                    scanner: Some(scanner),
                    serial,
                    status,
                    enumerated: false,
                    complete: false,
                    in_flight: 0,
                })
            }
            Err(failure) => {
                status.phase = ScanPhase::Failed;
                status.last_error = Some(db::bounded_error(format!("{failure:#}")));
                self.persist(&status)?;
                self.publish(true)?;
                return Err(failure);
            }
        }
        self.publish(true)
    }
    fn scan_tick(&mut self) -> Result<()> {
        let mut active = self
            .active
            .take()
            .context("scan tick without active scan")?;
        let previous_phase = active.status.phase;
        match advance(&mut self.connection, &mut active) {
            Ok(done) => {
                self.dirty = true;
                let transition = done || active.status.phase != previous_phase;
                if !done {
                    self.active = Some(active);
                }
                self.publish(transition)?;
            }
            Err(failure) => {
                if let Some(scanner) = active.scanner.take() {
                    let _ = scanner.shutdown();
                }
                active.status.phase = ScanPhase::Failed;
                active.status.last_error = Some(db::bounded_error(format!("{failure:#}")));
                self.persist(&active.status)?;
                self.cleanup_pending = true;
                self.publish(true)?;
            }
        }
        Ok(())
    }
}

fn advance(connection: &mut Connection, active: &mut Active) -> Result<bool> {
    let scanner = active.scanner.as_ref().context("scan worker missing")?;
    match scanner.try_recv() {
        Ok(ScanEvent::Candidates(candidates)) => {
            ensure!(
                !active.enumerated,
                "scanner returned candidates after enumeration"
            );
            db::discover(connection, active.serial, &mut active.status, &candidates)?;
        }
        Ok(ScanEvent::Enumerated(enumeration)) => {
            ensure!(!active.enumerated, "scanner finished enumeration twice");
            ensure!(
                enumeration.total == active.status.discovered,
                "scanner candidate count mismatch"
            );
            active.enumerated = true;
            active.complete = enumeration.complete;
            active.status.phase = ScanPhase::Indexing;
            active.status.total = Some(active.status.discovered);
            active.status.skipped = enumeration.skipped;
            active.status.failures += enumeration.failures;
            if let Some(failure) = enumeration.last_error {
                active.status.last_error = Some(db::bounded_error(failure));
            }
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            db::save_status(&transaction, &active.status)?;
            transaction.commit()?;
        }
        Ok(ScanEvent::Probed(results)) => {
            ensure!(
                active.enumerated && active.in_flight > 0 && results.len() == active.in_flight,
                "scanner returned an unexpected probe batch"
            );
            db::probed(connection, active.serial, &mut active.status, results)?;
            active.in_flight = 0;
        }
        Err(startup::TryRecvError::Empty) => {}
        Err(startup::TryRecvError::Disconnected) => {
            return Err(anyhow!("scanner disconnected before scan completion"));
        }
    }
    if active.enumerated && active.in_flight == 0 {
        if active.status.total == Some(active.status.processed) {
            active
                .scanner
                .take()
                .context("scan worker missing")?
                .shutdown()?;
            db::finish(
                connection,
                active.serial,
                &mut active.status,
                active.complete,
            )?;
            return Ok(true);
        }
        let probes = db::next_batch(connection, active.serial, &mut active.status)?;
        if !probes.is_empty() {
            let count = probes.len();
            match scanner.try_probe(probes) {
                Ok(()) => active.in_flight = count,
                Err(startup::TrySendError::Full(_)) => {} // persisted work remains pending
                Err(startup::TrySendError::Disconnected(_)) => {
                    return Err(anyhow!("scanner disconnected before metadata probing"));
                }
            }
        }
    }
    Ok(false)
}
