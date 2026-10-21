//! Listening statistics: local calendar periods and aggregation, computed on a worker thread with
//! its own read-only connection, never on the playback owner.
pub(crate) mod period;
pub(crate) mod report;
pub(crate) mod rows;
pub(crate) mod wrapped;

use crate::protocol::{ErrorCode, Reply, Response};
use crate::stats_types::PeriodRequest;
use anyhow::Result;
use period::{Date, SystemZone};
use report::Thresholds;
use rusqlite::{Connection, OpenFlags};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

const QUEUE_CAPACITY: usize = 8;

pub(crate) enum Request {
    Stats(PeriodRequest),
    Wrapped(i32),
    Years,
}

struct Work {
    request: Request,
    reply: oneshot::Sender<Reply>,
}

#[derive(Clone)]
pub(crate) struct Handle {
    sender: mpsc::SyncSender<Work>,
    stop: Arc<AtomicBool>,
}

pub(crate) struct StatsWorkers {
    handle: Handle,
    thread: Option<thread::JoinHandle<()>>,
}

impl StatsWorkers {
    pub(crate) fn start(database: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Work>(QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("stats".into())
            .spawn(move || {
                let mut connection = None;
                while !running.load(Ordering::Acquire) {
                    let work = match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(work) => work,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if work.reply.is_closed() {
                        continue;
                    }
                    let reply = open(&mut connection, &database)
                        .and_then(|connection| answer(connection, work.request))
                        .unwrap_or_else(failure);
                    let _ = work.reply.send(reply);
                }
            })
            .ok();
        Self {
            handle: Handle { sender, stop },
            thread,
        }
    }
    pub(crate) fn handle(&self) -> Handle {
        self.handle.clone()
    }
    pub(crate) fn stop(&self) {
        self.handle.stop.store(true, Ordering::Release);
    }
    pub(crate) fn shutdown(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for StatsWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Handle {
    pub(crate) async fn request(&self, request: Request) -> Reply {
        if self.stop.load(Ordering::Acquire) {
            return unavailable("stats worker stopped");
        }
        let (reply, receive) = oneshot::channel();
        if self.sender.try_send(Work { request, reply }).is_err() {
            return unavailable("stats request queue is full or unavailable");
        }
        receive
            .await
            .unwrap_or_else(|_| unavailable("stats request cancelled"))
    }
}

/// A failure the client can show. `Reply::Error` carries the owner's queue revision, which
/// the controller fills in.
enum Failure {
    BadAnchor,
    BadYear,
    Unavailable(String),
}

fn failure(error: Failure) -> Reply {
    match error {
        Failure::BadAnchor => Reply::Error {
            code: ErrorCode::InvalidCommand,
            message: "invalid period anchor".into(),
            queue_revision: 0,
        },
        Failure::BadYear => Reply::Error {
            code: ErrorCode::InvalidCommand,
            message: "invalid year".into(),
            queue_revision: 0,
        },
        Failure::Unavailable(message) => unavailable(&message),
    }
}

fn unavailable(message: &str) -> Reply {
    Reply::Error {
        code: ErrorCode::Unavailable,
        message: message.into(),
        queue_revision: 0,
    }
}

fn open<'a>(
    slot: &'a mut Option<Connection>,
    database: &PathBuf,
) -> Result<&'a Connection, Failure> {
    if slot.is_none() {
        let connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .and_then(|connection| {
            connection.busy_timeout(Duration::from_secs(5))?;
            Ok(connection)
        })
        .map_err(|error| Failure::Unavailable(format!("opening stats database: {error}")))?;
        *slot = Some(connection);
    }
    Ok(slot.as_ref().expect("opened above"))
}

fn answer(connection: &Connection, request: Request) -> Result<Reply, Failure> {
    let unavailable = |error: anyhow::Error| Failure::Unavailable(format!("{error:#}"));
    let anchor = match &request {
        Request::Stats(period) => Some(Date::parse(&period.anchor).ok_or(Failure::BadAnchor)?),
        Request::Wrapped(year) if !(1970..=9999).contains(year) => return Err(Failure::BadYear),
        _ => None,
    };
    let stats_revision: u64 = connection
        .query_row("SELECT writes FROM history_control", [], |row| row.get(0))
        .map_err(|error| unavailable(error.into()))?;
    let (rows, library) = rows::load(connection).map_err(unavailable)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let zone = SystemZone;
    let thresholds = Thresholds::default();
    let result = match request {
        Request::Stats(period) => {
            // All starts at the earliest row of any kind, so skips, radio and open rows
            // before the first counted play still count towards its listened time.
            let earliest = rows.first().map(|row| row.played_at);
            let bounds = period::bounds(
                &zone,
                period.kind,
                anchor.expect("parsed above"),
                earliest,
                now,
            );
            Response::Stats {
                stats_revision,
                report: Box::new(report::report(
                    &rows,
                    &library,
                    &zone,
                    bounds,
                    period.kind,
                    now,
                    &thresholds,
                )),
            }
        }
        Request::Wrapped(year) => Response::Wrapped {
            stats_revision,
            report: Box::new(wrapped::wrapped(
                &rows,
                &library,
                &zone,
                year,
                now,
                &thresholds,
            )),
        },
        Request::Years => Response::StatsYears {
            years: wrapped::years(&rows, &zone),
        },
    };
    Ok(Reply::Ok { result })
}

#[cfg(test)]
mod tests;
