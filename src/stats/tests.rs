use super::*;
use crate::protocol::{ErrorCode, Reply, Response};
use crate::stats_types::{PeriodKind, PeriodRequest};
use crate::store::Store;
use std::path::PathBuf;

struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tyranny-likes-smoke-stats-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let _store = Store::open(&path).unwrap();
        Self(path)
    }
    fn execute(&self, sql: &str) {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = self.0.clone().into_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(name);
        }
    }
}

const PLAY: &str = "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,source_kind,source_name,listened_ms,ended_at,end_reason,counted) VALUES
    (1790000000,1,NULL,'/m/1','Song','Band','A',200000,NULL,NULL,150000,1790000150,'finished',1);
    UPDATE history_control SET writes=writes+1;";

fn all() -> Request {
    Request::Stats(PeriodRequest {
        kind: PeriodKind::All,
        anchor: "2026-10-21".into(),
    })
}

#[tokio::test]
async fn stats_are_served_by_the_worker_without_an_owner() {
    let database = Database::new();
    database.execute(PLAY);
    let workers = StatsWorkers::start(database.0.clone());
    match workers.handle().request(all()).await {
        Reply::Ok {
            result:
                Response::Stats {
                    stats_revision,
                    report,
                },
        } => {
            assert_eq!(stats_revision, 1);
            assert_eq!(report.plays, 1);
        }
        reply => panic!("unexpected reply: {reply:?}"),
    }
    match workers.handle().request(Request::Years).await {
        Reply::Ok {
            result: Response::StatsYears { years },
        } => assert_eq!(years, vec![2026]),
        reply => panic!("unexpected reply: {reply:?}"),
    }
    match workers.handle().request(Request::Wrapped(2026)).await {
        Reply::Ok {
            result:
                Response::Wrapped {
                    stats_revision: 1,
                    report,
                },
        } => assert_eq!(report.plays, 1),
        reply => panic!("unexpected reply: {reply:?}"),
    }
    workers.shutdown();
}

fn listened_ms(reply: Reply) -> u64 {
    match reply {
        Reply::Ok {
            result: Response::Stats { report, .. },
        } => report.listened_ms,
        reply => panic!("unexpected reply: {reply:?}"),
    }
}

fn year_2026() -> Request {
    Request::Stats(PeriodRequest {
        kind: PeriodKind::Year,
        anchor: "2026-01-01".into(),
    })
}

#[tokio::test]
async fn all_runs_from_the_earliest_row_not_the_first_counted_play() {
    let database = Database::new();
    // A skip before the first counted play.
    database.execute(
        "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,source_kind,source_name,listened_ms,ended_at,end_reason,counted) VALUES
        (1789990000,2,NULL,'/m/2','Other','Band','A',200000,NULL,NULL,30000,1789990030,'skipped',0);",
    );
    database.execute(PLAY);
    let workers = StatsWorkers::start(database.0.clone());
    let all = listened_ms(workers.handle().request(all()).await);
    let year = listened_ms(workers.handle().request(year_2026()).await);
    assert_eq!((all, year), (180_000, 180_000));
    workers.shutdown();
}

#[tokio::test]
async fn all_counts_radio_time_when_nothing_has_counted() {
    let database = Database::new();
    database.execute(
        "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,source_kind,source_name,listened_ms,ended_at,end_reason,counted) VALUES
        (1790000000,1,NULL,'http://s','','','',NULL,'stream','FM',45000,1790000045,'stopped',0);",
    );
    let workers = StatsWorkers::start(database.0.clone());
    assert_eq!(listened_ms(workers.handle().request(all()).await), 45_000);
    workers.shutdown();
}

#[tokio::test]
async fn stats_years_reads_counted_play_times_only() {
    let database = Database::new();
    database.execute(PLAY);
    // An uncounted 2025 play does not make its year, and the library is never read.
    database.execute(
        "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,source_kind,source_name,listened_ms,ended_at,end_reason,counted) VALUES
        (1750000000,2,NULL,'/m/2','Other','Band','A',200000,NULL,NULL,30000,1750000030,'skipped',0),
        (1720000000,3,NULL,'/m/3','Old','Band','A',200000,NULL,NULL,150000,1720000150,'finished',1);
        DROP TABLE library_tracks;",
    );
    let workers = StatsWorkers::start(database.0.clone());
    match workers.handle().request(Request::Years).await {
        Reply::Ok {
            result: Response::StatsYears { years },
        } => assert_eq!(years, vec![2026, 2024]),
        reply => panic!("unexpected reply: {reply:?}"),
    }
    workers.shutdown();
}

#[tokio::test]
async fn a_bad_anchor_is_an_invalid_command() {
    let database = Database::new();
    let workers = StatsWorkers::start(database.0.clone());
    let request = Request::Stats(PeriodRequest {
        kind: PeriodKind::Day,
        anchor: "2026-02-30".into(),
    });
    match workers.handle().request(request).await {
        Reply::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::InvalidCommand);
            assert_eq!(message, "invalid period anchor");
        }
        reply => panic!("unexpected reply: {reply:?}"),
    }
    workers.shutdown();
}

#[tokio::test]
async fn a_missing_database_is_unavailable_and_stopped_workers_refuse() {
    let missing = std::env::temp_dir().join(format!("absent-{}.sqlite", uuid::Uuid::new_v4()));
    let workers = StatsWorkers::start(missing);
    let handle = workers.handle();
    assert!(matches!(
        handle.request(all()).await,
        Reply::Error {
            code: ErrorCode::Unavailable,
            ..
        }
    ));
    workers.shutdown();
    assert!(matches!(
        handle.request(all()).await,
        Reply::Error {
            code: ErrorCode::Unavailable,
            ..
        }
    ));
}

#[tokio::test]
async fn wrapped_years_outside_the_calendar_are_rejected_before_any_work() {
    let database = Database::new();
    let workers = StatsWorkers::start(database.0.clone());
    for year in [1969, 10_000, i32::MAX, i32::MIN] {
        match workers.handle().request(Request::Wrapped(year)).await {
            Reply::Error { code, message, .. } => {
                assert_eq!(code, ErrorCode::InvalidCommand, "{year}");
                assert_eq!(message, "invalid year");
            }
            reply => panic!("{year} was not rejected: {reply:?}"),
        }
    }
    for year in [1970, 9999] {
        assert!(matches!(
            workers.handle().request(Request::Wrapped(year)).await,
            Reply::Ok { .. }
        ));
    }
    // The only stats thread is still alive.
    assert!(matches!(
        workers.handle().request(all()).await,
        Reply::Ok { .. }
    ));
    workers.shutdown();
}
