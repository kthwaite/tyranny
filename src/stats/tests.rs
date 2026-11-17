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

fn open_at(writer: &mut Store, played_at: i64) -> i64 {
    writer
        .open_play(&crate::store::PlayRecord {
            entry_id: 1,
            track_id: None,
            path: "/m/song".into(),
            title: "Song".into(),
            artist: "Band".into(),
            album: "A".into(),
            duration_ms: None,
            source: None,
            played_at,
            raw_title: None,
        })
        .unwrap()
}

// The scalar function receives a value already read from the report's WAL snapshot.
// Count the supplied rows on the writer connection before returning that unchanged value.
fn count_during_read(database: &Database, writer: Store, rows: Vec<i64>, view: &str) -> Connection {
    let reader =
        Connection::open_with_flags(&database.0, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let pending = std::cell::RefCell::new(Some((writer, rows)));
    reader
        .create_scalar_function(
            "count_pending",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                if let Some((mut writer, rows)) = pending.take() {
                    for row in rows {
                        writer.count_play(row).unwrap();
                    }
                }
                ctx.get::<i64>(0)
            },
        )
        .unwrap();
    reader.execute_batch(view).unwrap();
    reader
}

fn assert_report_snapshot(request: impl Fn() -> Request) {
    let database = Database::new();
    let mut writer = Store::open(&database.0).unwrap();
    let row = open_at(&mut writer, 1_720_000_000); // 2024
    let mut reader = count_during_read(
        &database,
        writer,
        vec![row],
        "CREATE TEMP VIEW history_control AS
         SELECT count_pending(writes) AS writes FROM main.history_control",
    );
    for expected in [(1, 0), (2, 1)] {
        let reply = answer(&mut reader, request()).unwrap_or_else(failure);
        let pair = match reply {
            Reply::Ok {
                result:
                    Response::Stats {
                        stats_revision,
                        report,
                    },
            } => (stats_revision, report.plays),
            Reply::Ok {
                result:
                    Response::Wrapped {
                        stats_revision,
                        report,
                    },
            } => (stats_revision, report.plays),
            other => panic!("unexpected reply: {other:?}"),
        };
        assert_eq!(pair, expected);
    }
}

#[test]
fn stats_returns_the_revision_of_the_reported_plays() {
    assert_report_snapshot(all);
}

#[test]
fn wrapped_returns_the_revision_of_the_reported_plays() {
    assert_report_snapshot(|| Request::Wrapped(2024));
}

#[test]
fn years_never_mix_old_bounds_with_new_counted_plays() {
    let database = Database::new();
    let mut writer = Store::open(&database.0).unwrap();
    for time in [1_625_000_000, 1_720_000_000] {
        // 2021, 2024
        let row = open_at(&mut writer, time);
        writer.count_play(row).unwrap();
    }
    let outside = open_at(&mut writer, 1_593_000_000); // 2020
    let inside = open_at(&mut writer, 1_656_000_000); // 2022
    let mut reader = count_during_read(
        &database,
        writer,
        vec![outside, inside],
        "CREATE TEMP VIEW play_history AS
         SELECT count_pending(played_at) AS played_at, counted FROM main.play_history",
    );
    // 2022 without 2020 never existed: the outside year counted first.
    for expected in [vec![2024, 2021], vec![2024, 2022, 2021, 2020]] {
        match answer(&mut reader, Request::Years).unwrap_or_else(failure) {
            Reply::Ok {
                result: Response::StatsYears { years },
            } => assert_eq!(years, expected),
            other => panic!("unexpected reply: {other:?}"),
        }
    }
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

mod snapshot_search {
    use super::*;
    use crate::store::{EndReason, PlayRecord};
    use proptest::prelude::*;
    use rusqlite::functions::FunctionFlags;
    use std::cell::RefCell;

    // Year offset from 2020, whether it counted, milliseconds listened.
    type Play = (u8, bool, u16);

    fn plays() -> impl Strategy<Value = Vec<Play>> {
        prop::collection::vec((0..5u8, any::<bool>(), 0..3001u16), 0..7)
    }

    fn write_plays(store: &mut Store, plays: &[Play]) {
        for &(year, counted, ms) in plays {
            // Mid-year, far from any leap day, calendar boundary or current time.
            let played_at = 1_593_604_800 + i64::from(year) * 365 * 86_400;
            let row = store
                .open_play(&PlayRecord {
                    entry_id: 1,
                    track_id: None,
                    path: "/m/song".into(),
                    title: "Song".into(),
                    artist: "Band".into(),
                    album: "A".into(),
                    duration_ms: None,
                    source: None,
                    played_at,
                    raw_title: None,
                })
                .unwrap();
            if counted {
                store.count_play(row).unwrap();
            }
            store
                .close_play(row, u64::from(ms), EndReason::Finished, played_at + 3)
                .unwrap();
        }
    }

    fn commit_during_read(reader: &Connection, writer: Store, batch: Vec<Play>, view: &str) {
        let pending = RefCell::new(Some((writer, batch)));
        reader
            .create_scalar_function("step_writer", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
                if let Some((mut writer, batch)) = pending.take() {
                    write_plays(&mut writer, &batch);
                }
                ctx.get::<i64>(0)
            })
            .unwrap();
        reader.execute_batch(view).unwrap();
    }

    fn expected_report(plays: &[Play], wrapped: Option<u8>) -> (u64, u64, u64) {
        let mut revision = 0;
        let mut count = 0;
        let mut time = 0;
        for &(year, counted, ms) in plays {
            revision += 2 + u64::from(counted); // open, optional count, close
            if wrapped.is_none_or(|selected| year == selected) {
                count += u64::from(counted);
                time += u64::from(ms);
            }
        }
        (revision, count, time)
    }

    fn observed_report(reader: &mut Connection, wrapped: Option<u8>) -> (u64, u64, u64) {
        let request = wrapped.map_or_else(all, |y| Request::Wrapped(2020 + i32::from(y)));
        match answer(reader, request).unwrap_or_else(failure) {
            Reply::Ok {
                result:
                    Response::Stats {
                        stats_revision,
                        report,
                    },
            } => (stats_revision, report.plays, report.listened_ms),
            Reply::Ok {
                result:
                    Response::Wrapped {
                        stats_revision,
                        report,
                    },
            } => (stats_revision, report.plays, report.listened_ms),
            reply => panic!("unexpected reply: {reply:?}"),
        }
    }

    fn year_list(plays: &[Play]) -> Vec<i32> {
        (0..5)
            .rev()
            .filter(|&y| plays.iter().any(|&(year, counted, _)| year == y && counted))
            .map(|y| 2020 + i32::from(y))
            .collect()
    }

    fn observed_years(reader: &mut Connection) -> Vec<i32> {
        match answer(reader, Request::Years).unwrap_or_else(failure) {
            Reply::Ok {
                result: Response::StatsYears { years },
            } => years,
            reply => panic!("unexpected reply: {reply:?}"),
        }
    }

    proptest! {
        #[test]
        fn report_uses_one_snapshot(
            before in plays(), during in plays(), wrapped in prop::option::of(0..5u8),
        ) {
            let database = Database::new();
            let mut writer = Store::open(&database.0).unwrap();
            write_plays(&mut writer, &before);
            let mut reader = Connection::open_with_flags(&database.0, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let after = [before.as_slice(), &during].concat();
            commit_during_read(&reader, writer, during,
                "CREATE TEMP VIEW history_control AS SELECT step_writer(writes) AS writes FROM main.history_control");
            // The first SELECT has read its input from the before snapshot when the writer runs.
            prop_assert_eq!(observed_report(&mut reader, wrapped), expected_report(&before, wrapped));
            prop_assert_eq!(observed_report(&mut reader, wrapped), expected_report(&after, wrapped));
        }

        #[test]
        fn years_use_one_snapshot(
            mut before in plays(), mut during in plays(), gap in 2..4u8,
        ) {
            // Prime the endpoints but leave a gap. Inserting the outside year *first*
            // makes observing the gap without that outside year an impossible snapshot.
            before.retain(|&(y, _, _)| y > 0 && y != gap);
            before.extend([(1, true, 1), (4, true, 1)]);
            during.splice(0..0, [(0, true, 1), (gap, true, 1)]);
            let database = Database::new();
            let mut writer = Store::open(&database.0).unwrap();
            write_plays(&mut writer, &before);
            let mut reader = Connection::open_with_flags(&database.0, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let after = [before.as_slice(), &during].concat();
            commit_during_read(&reader, writer, during,
                "CREATE TEMP VIEW play_history AS SELECT step_writer(played_at) AS played_at,counted FROM main.play_history");
            prop_assert_eq!(observed_years(&mut reader), year_list(&before));
            prop_assert_eq!(observed_years(&mut reader), year_list(&after));
        }
    }
}
