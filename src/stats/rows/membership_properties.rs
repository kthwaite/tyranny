//! Retained SQLite identities vs current membership in Stats and Wrapped links.
use super::load;
use crate::{library, stats::{period::{Bounds, FixedZone}, report::{self, Thresholds}, wrapped}, stats_types::PeriodKind};
use proptest::prelude::*;
use rusqlite::{Connection, params};

const NAMES: [&str; 4] = ["Band", " BAND ", "Curator", "Guest"];
// (track artist, album artist, root, title). Two different artist IDs share a normalized key.
const TRACKS: [(usize, usize, usize, &str); 4] = [
    (0, 0, 0, "Single"), (1, 2, 1, "Single"), (0, 2, 0, "Second"), (3, 0, 1, "Guest spot"),
];
const HISTORY: [(&str, &str, Option<u64>); 4] = [
    ("Band", "Single", Some(1)), ("Band", "Second", Some(3)), ("Guest", "Guest spot", Some(4)),
    // Unindexed playback by the fixture's album-only artist.
    ("Curator", "Intro", None),
];
const DAY: i64 = 1_767_225_600; // 2026-01-01 UTC

#[derive(Clone, Debug)]
enum Op {
    Remove(usize),
    Scan { root: usize, seen: u8 },
    Return(usize),
}
fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..4usize).prop_map(Op::Remove),
        (0..2usize, 0..16u8).prop_map(|(root, seen)| Op::Scan { root, seen }),
        (0..4usize).prop_map(Op::Return),
    ]
}

struct World {
    db: Connection,
    // Model real-world events, not the watermark representation.
    present: [bool; 4],
    removed: [bool; 4],
    serial: i64,
}
impl World {
    fn new() -> Self {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        crate::store::migrate(&mut db, 11).unwrap();
        db.execute_batch("INSERT INTO library_roots(id,path,search,completed_scan) VALUES (1,'/a','/a',1),(2,'/b','/b',1)").unwrap();
        for (i, name) in NAMES.into_iter().enumerate() {
            db.execute("INSERT INTO library_artists(id,identity,name,search) VALUES(?1,?1,?2,?3)", params![i + 1, name, name.to_lowercase()]).unwrap();
        }
        for (i, (artist, album_artist, root, title)) in TRACKS.into_iter().enumerate() {
            db.execute("INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES(?1,?2,?3,1,1)", params![i+1, root+1, format!("/{}/{}", ["a", "b"][root], i)]).unwrap();
            db.execute("INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation) VALUES(?1,?1,?2,'Album',?3,'album','album',0)", params![i+1, album_artist+1, NAMES[album_artist]]).unwrap();
            let metadata = serde_json::json!({"title": title, "artist": NAMES[artist]}).to_string();
            db.execute("INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number) VALUES(?1,?1,?2,?1,?3,?4,?4,1,1,1)", params![i+1, artist+1, metadata, title]).unwrap();
        }
        for (artist, title, id) in HISTORY {
            // Five skips make Wrapped's skipped-track link observable too.
            for i in 0..6 {
                db.execute("INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,listened_ms,end_reason,counted) VALUES(?1,1,?2,'/old',?3,?4,'',9000,?5,?6)", params![DAY+i, id, title, artist, if i==0 {"finished"} else {"skipped"}, i==0]).unwrap();
            }
        }
        Self { db, present: [true; 4], removed: [false; 4], serial: 1 }
    }
    fn apply(&mut self, op: Op) {
        self.serial += 1;
        match op {
            Op::Remove(i) => {
                if self.present[i] && !self.removed[i] {
                    self.db.execute("UPDATE library_assets SET excluded=1,indexed=0 WHERE id=?1", [i+1]).unwrap();
                    self.removed[i] = true;
                }
            }
            Op::Scan { root, seen } => {
                for (i, _) in TRACKS.iter().enumerate().filter(|(_, e)| e.2 == root) {
                    self.present[i] = seen & (1 << i) != 0;
                    if self.present[i] {
                        self.db.execute("UPDATE library_assets SET last_seen_scan=?1 WHERE id=?2 AND excluded=0", params![self.serial, i+1]).unwrap();
                    }
                }
                self.db.execute("UPDATE library_roots SET completed_scan=?1 WHERE id=?2", params![self.serial, root+1]).unwrap();
            }
            Op::Return(i) => {
                self.present[i] = true;
                self.db.execute("UPDATE library_assets SET last_seen_scan=?1 WHERE id=?2 AND excluded=0", params![self.serial, i+1]).unwrap();
            }
        }
    }
    fn check(&self) {
        let visible: Vec<_> = TRACKS.iter().enumerate().filter(|(i, _)| self.present[*i] && !self.removed[*i]).collect();
        for i in 0..TRACKS.len() {
            assert_eq!(library::resolve_track(&self.db, i as u64 + 1, 1).is_ok(), self.present[i] && !self.removed[i]);
        }
        fn unique(ids: Vec<u64>) -> Option<u64> { if let [id] = ids[..] { Some(id) } else { None } }
        let track = |artist: &str, title: &str| unique(visible.iter().filter_map(|(i, (a, _, _, t))|
            (NAMES[*a].trim().eq_ignore_ascii_case(artist) && t.eq_ignore_ascii_case(title)).then_some(*i as u64+1)
        ).collect());
        let artist = |name: &str| unique(NAMES.iter().enumerate().filter_map(|(i, n)|
            (n.trim().eq_ignore_ascii_case(name) && visible.iter().any(|(_, (a, aa, _, _))| i==*a || i==*aa)).then_some(i as u64+1)
        ).collect());
        let (plays, library) = load(&self.db).unwrap();
        let thresholds = Thresholds { shallow_plays_floor: 1, lapsed_plays_floor: 1, scale_plays: 0, ..Default::default() };
        let later = DAY + 365 * 86_400;
        let report = report::report(&plays, &library, &FixedZone(0), Bounds {start: DAY, end: DAY+86_400}, PeriodKind::Day, later, &thresholds);
        let wrapped = wrapped::wrapped(&plays, &library, &FixedZone(0), 2026, later, &thresholds);
        assert_eq!((report.plays, wrapped.plays), (4, 4));
        assert_eq!((report.listened_ms, wrapped.listened_ms), (216_000, 216_000));
        for rows in [&report.top_tracks_by_plays, &report.top_tracks_by_time, &wrapped.top_tracks] {
            assert_eq!(rows.len(), 4);
            for row in rows { assert_eq!(row.track_id, track(&row.artist, &row.title), "track {}", row.title); }
        }
        assert_eq!(report.most_skipped.len(), 4);
        let skipped = wrapped.most_skipped.as_ref().expect("five skips");
        for row in report.most_skipped.iter().chain([skipped]) {
            assert_eq!(row.track_id, track(&row.artist, &row.title));
        }
        for rows in [&report.top_artists_by_plays, &report.top_artists_by_time, &report.new_artists] {
            assert_eq!(rows.len(), 3);
            for row in rows { assert_eq!(row.artist_id, artist(&row.name), "artist {}", row.name); }
        }
        assert_eq!(report.lapsed_loves.len(), 3);
        for row in report.lapsed_loves { assert_eq!(row.artist_id, artist(&row.name)); }
        assert_eq!(report.shallow_dives.len(), 3);
        for row in report.shallow_dives {
            let id = artist(&row.name);
            assert_eq!(row.artist_id, id);
            let count = id.map(|id| visible.iter().filter(|(_, t)| t.0 as u64+1 == id).count() as u64);
            assert_eq!(row.library_tracks, count.filter(|&n| n!=0), "track artist {}", row.name);
        }
    }
}

proptest! {
    #[test]
    fn report_membership_tracks_visible_credits(ops in prop::collection::vec(op(), 0..16)) {
        let mut world = World::new();
        world.check();
        // Full visibility -> unique -> ambiguous -> unique; neither the duplicate nor its plays are deleted.
        for step in ops.into_iter().chain([Op::Scan{root:1,seen:8}, Op::Return(1), Op::Remove(1)]) {
            world.apply(step);
            world.check();
        }
    }
}
