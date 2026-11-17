//! Compare completed scans with a rebuild and a recomputation over raw tags. IDs
//! are local database handles; compare partitions by their sorted source paths.
use super::*;
use crate::{
    library::query,
    library_types::{LibraryCommand, LibraryReply, PageRequest, ScanStatus},
    protocol::TrackSelection,
    scanner::{Candidate, ProbeResult},
};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

const SLOTS: usize = 6;
fn root(slot: usize) -> usize { slot / 3 + 1 }
fn path(slot: usize) -> String {
    format!("/root{}/{}/{}.flac", root(slot), if slot % 3 == 2 { "disc2" } else { "disc1" }, slot)
}

#[derive(Clone, Copy, Debug)]
struct Tags {
    artist: &'static str,
    album: &'static str,
    release: Option<&'static str>,
    credit: Option<&'static str>,
    flag: Option<bool>,
    discs: u32,
}
fn plain(artist: &'static str, release: Option<&'static str>) -> Tags {
    Tags { artist, album: "Sampler", release, credit: None, flag: None, discs: 1 }
}
impl Tags {
    fn credit(self, inferred: bool) -> (&'static str, bool) {
        let va = self.credit == Some("VA");
        let compilation = self.flag.unwrap_or(va || inferred);
        let album_artist = match (self.credit, compilation) {
            (Some("VA"), _) | (None, true) => "Various Artists",
            (Some(artist), _) => artist,
            (None, false) => self.artist,
        };
        (album_artist, compilation)
    }
    fn metadata(self, path: &str) -> TrackMetadata {
        let (artist, compilation) = self.credit(false);
        TrackMetadata {
            title: path.to_owned(), artist: self.artist.into(), album: self.album.into(),
            musicbrainz_release_id: self.release.map(str::to_owned),
            album_artist_tag: self.credit.map(str::to_owned), compilation_tag: self.flag,
            album_artist: artist.into(), compilation, disc_total: Some(self.discs),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct File { tags: Tags, modified: i64, excluded: bool }
type Files = [Option<File>; SLOTS];

fn database() -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    crate::store::migrate(&mut connection, 11).unwrap();
    connection.execute_batch("INSERT INTO library_roots(id,path,search) VALUES
        (1,'/root1','/root1'),(2,'/root2','/root2'); UPDATE library_state SET root_count=2;").unwrap();
    initialize(&connection).unwrap();
    connection
}

/// Drive the coordinator's real DB event sequence. All candidates have equal
/// size: same-path replacements must be picked up from their modification time.
fn scan(c: &mut Connection, files: &Files, root_id: usize, only: Option<usize>) {
    let full = only.is_none();
    let tx = c.transaction().unwrap();
    let (serial, mut status) = if full { db::begin_scan(&tx, root_id as u64).unwrap() } else {
        (db::begin_paths(&tx).unwrap(), ScanStatus { root_id: Some(root_id as u64), ..Default::default() })
    };
    tx.commit().unwrap();
    begin(c, root_id as u64).unwrap();
    let mut work = Reconciliation::default();
    while !work.seed(c).unwrap() {}
    let mut metadata = BTreeMap::new();
    let mut candidates = vec![];
    for (slot, file) in files.iter().enumerate() {
        let Some(file) = file else { continue };
        if root(slot) != root_id || only.is_some_and(|s| s != slot) { continue }
        let name = path(slot);
        candidates.push(Candidate { path: name.clone().into(), size: 100, modified_ns: file.modified, created_ns: None });
        metadata.insert(name.clone(), file.tags.metadata(&name));
    }
    db::discover(c, serial, &mut status, &candidates, full).unwrap();
    status.total = Some(status.discovered);
    while status.processed < status.discovered {
        let candidates = db::next_batch(c, serial, &mut status, full).unwrap();
        if !candidates.is_empty() {
            let results = candidates.into_iter().map(|candidate| ProbeResult {
                result: Ok(metadata.remove(candidate.path.to_str().unwrap()).unwrap()), candidate,
            }).collect();
            db::probed(c, serial, &mut status, results, full).unwrap();
        }
    }
    if full || status.discovered != 0 {
        let mut ticks = 0;
        while !work.step(c, root_id as u64, serial, full).unwrap() {
            ticks += 1;
            assert!(ticks < 300, "unfinished reconciliation");
        }
    }
    if full { db::finish(c, serial, &mut status, true).unwrap() }
    else { db::finish_paths(c, serial, &status).unwrap() }
}

#[derive(Debug, PartialEq, Eq, Default)]
struct View {
    // Member paths -> title(s) on visible tracks, effective credit, release, and disc count.
    albums: BTreeMap<Vec<String>, (BTreeSet<String>, String, bool, Option<String>, u64)>,
    artists: BTreeMap<String, u64>,
}
fn page() -> PageRequest { PageRequest { revision: None, query: String::new(), offset: 0, limit: 500 } }
fn view(c: &mut Connection) -> View {
    let mut view = View::default();
    let LibraryReply::Artists(artists) = query::execute(c, LibraryCommand::Artists(page())).unwrap() else { unreachable!() };
    let LibraryReply::Albums(albums) = query::execute(c, LibraryCommand::Albums { artist_id: None, page: page() }).unwrap() else { unreachable!() };
    assert_eq!(artists.total, artists.entries.len() as u64);
    assert_eq!(albums.total, albums.entries.len() as u64);
    for artist in artists.entries { assert!(view.artists.insert(artist.name, artist.album_count).is_none()); }
    for album in albums.entries {
        let LibraryReply::Tracks(tracks) = query::execute(c, LibraryCommand::Tracks { album_id: Some(album.id), page: page() }).unwrap() else { unreachable!() };
        assert_eq!(album.track_count, tracks.total);
        assert_eq!(album.track_count, tracks.entries.len() as u64);
        let mut paths = vec![];
        let mut titles = BTreeSet::new();
        for track in tracks.entries {
            assert_eq!((track.metadata.album_artist.as_str(), track.metadata.compilation), (album.album_artist.as_str(), album.compilation));
            paths.push(track.path);
            titles.insert(track.metadata.album);
        }
        // Conflicting local spellings for one release need not choose a specific
        // winner. A title once supplied by a departed track is not a valid one.
        assert!(titles.contains(&album.title), "cached title {:?} not among visible member titles {titles:?}; paths {paths:?}", album.title);
        paths.sort();
        assert!(view.albums.insert(paths, (titles, album.album_artist, album.compilation, album.musicbrainz_release_id, album.disc_count)).is_none());
    }
    view
}

/// Partition visible raw inputs first by release (or exact folder + title).
/// An untagged track borrows a release only if the local raw tags identify one.
/// Infer compilation by counting distinct eligible artists, not by replaying
/// DB seeds, interned IDs, phased updates, scan serials or row ordering.
fn model(files: &Files) -> View {
    let visible: Vec<_> = files.iter().enumerate().filter_map(|(s, f)| {
        f.filter(|f| !f.excluded).map(|f| (path(s), f.tags))
    }).collect();
    let mut groups = BTreeMap::new();
    for (p, tags) in &visible {
        let dir = Path::new(p).parent().unwrap();
        let ids: BTreeSet<_> = visible.iter().filter(|(other, t)| Path::new(other).parent() == Some(dir) && t.album == tags.album)
            .filter_map(|(_, t)| t.release).collect();
        let release = tags.release.or_else(|| (ids.len() == 1).then(|| *ids.first().unwrap()));
        let folder = release.is_none().then_some((dir, tags.album));
        groups.entry((release, folder)).or_insert_with(Vec::new).push((p, tags, release.is_none() && ids.len() > 1));
    }
    let mut view = View::default();
    for ((release, _), group) in groups {
        let eligible: BTreeSet<_> = group.iter().map(|(_, t, _)| t.artist.to_lowercase()).filter(|a| a != "unknown artist").collect();
        let infer = eligible.len() >= 2 && group.iter().all(|(_, t, ambiguous)| {
            !ambiguous && t.flag != Some(false) && t.credit.is_none_or(|c| c == "VA") && t.album != "Unknown Album"
        });
        let mut credits = BTreeMap::new();
        for (p, t, _) in group { credits.entry(t.credit(infer)).or_insert_with(Vec::new).push((p, t)); }
        for ((artist, compilation), members) in credits {
            let paths = members.iter().map(|(p, _)| (*p).clone()).collect::<BTreeSet<_>>().into_iter().collect();
            let titles = members.iter().map(|(_, t)| t.album.to_owned()).collect();
            let discs = members.iter().map(|(_, t)| t.discs).max().unwrap() as u64;
            view.albums.insert(paths, (titles, artist.into(), compilation, release.map(str::to_owned), discs));
            *view.artists.entry(artist.into()).or_default() += 1;
        }
    }
    view
}

#[derive(Clone, Debug)]
enum Edit { Put(usize, Tags, bool), Unlink(usize), Exclude(usize), Rescan(usize) }
fn check(history: &[Edit], reverse: bool) {
    let mut files: Files = [None; SLOTS];
    let mut c = database();
    for (n, edit) in history.iter().enumerate() {
        let (slot, only) = match *edit {
            Edit::Put(s, tags, single) => {
                let excluded = files[s].is_some_and(|f| f.excluded);
                files[s] = Some(File { tags, modified: n as i64 + 1, excluded });
                (s, single.then_some(s))
            }
            Edit::Unlink(s) => {
                // Exclusion belongs to a path even if the file is replaced. Keep
                // excluded files present to exercise rediscovery of tombstones.
                if !files[s].is_some_and(|f| f.excluded) { files[s] = None; }
                (s, None)
            }
            Edit::Exclude(s) => {
                if let Some(f) = &mut files[s] && !f.excluded {
                    let LibraryReply::Tracks(tracks) = query::execute(&mut c, LibraryCommand::Tracks { album_id: None, page: page() }).unwrap() else { unreachable!() };
                    let id = tracks.entries.iter().find(|t| t.path == path(s)).unwrap().id;
                    db::remove_tracks(&mut c, &TrackSelection::Ids { library_revision: tracks.revision, track_ids: vec![id] }).unwrap();
                    f.excluded = true;
                }
                (s, None)
            }
            Edit::Rescan(s) => (s, None),
        };
        scan(&mut c, &files, root(slot), only);
        assert_eq!(view(&mut c), model(&files), "after step {n}: {edit:?}");
    }
    let mut rebuilt = database();
    let visible = files.map(|f| f.filter(|f| !f.excluded));
    for r in if reverse { [2,1] } else { [1,2] } { scan(&mut rebuilt, &visible, r, None); }
    assert_eq!(view(&mut c), view(&mut rebuilt), "incremental vs fresh root scans");
}

fn tags() -> impl Strategy<Value = Tags> {
    (prop::sample::select(vec!["Ann", "Bob", "ANN", "Unknown Artist"]),
     prop::sample::select(vec!["Sampler", "Sampler", "Box", "Unknown Album"]),
     prop::sample::select(vec![None, None, Some("release-a"), Some("release-z")]),
     prop::sample::select(vec![None, None, None, Some("VA"), Some("Owner")]),
     prop::sample::select(vec![None, None, None, Some(false), Some(true)]), 1..=3_u32)
    .prop_map(|(artist, album, release, credit, flag, discs)| Tags { artist, album, release, credit, flag, discs })
}
fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![7 => (0..SLOTS, tags(), any::<bool>()).prop_map(|(s,t,b)| Edit::Put(s,t,b)),
        2 => (0..SLOTS).prop_map(Edit::Unlink), 1 => (0..SLOTS).prop_map(Edit::Exclude),
        1 => (0..SLOTS).prop_map(Edit::Rescan)]
}
proptest! {
    #[test]
    fn incremental_matches_rebuild(history in prop::collection::vec(edit(), 2..14), reverse in any::<bool>()) {
        check(&history, reverse);
    }
}

#[test]
fn departing_shared_release_member_matrix() {
    use Edit::*;
    for other in [1, 2, 3] { // same folder, same root/different disc, other root
        for exclusion in [false, true] {
            for single in [false, true] {
                for replaced in [false, true] {
                    let a = plain("Ann", Some("release-a"));
                    let b = plain("Bob", Some("release-a"));
                    let mut h = vec![Put(0, a, single), Put(other, b, single)];
                    if replaced { h.push(Put(other, plain("Bob", Some("release-z")), !single)); }
                    h.push(if exclusion { Exclude(other) } else { Unlink(other) });
                    check(&h, true);
                }
            }
        }
    }
}

#[test]
fn visible_title_after_departure_matrix() {
    use Edit::*;
    for other in [1, 2, 3] {
        for single in [false, true] {
            for via_retag in [false, true] {
                let a = plain("Ann", Some("release-a"));
                let b = Tags { album: "Deluxe", ..a };
                let departure = if via_retag { Put(other, Tags { release: Some("release-z"), ..b }, single) } else { Unlink(other) };
                check(&[Put(0, a, single), Put(other, b, single), departure], true);
            }
        }
    }
}
