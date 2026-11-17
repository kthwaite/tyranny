//! Replay the same logical drag/edit history in two ID spaces, not two copies
//! of the SQL algorithm. Keep retired bindings so stale references stay stale.
use super::*;
use anyhow::{Context, ensure};
use proptest::{
    collection::vec,
    prelude::*,
    test_runner::{Config, TestRunner},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
};

#[derive(Clone, Debug)]
enum Select {
    Ranks(Vec<u8>), // distinct live rows, in request (not necessarily list) order
    AllBackwards,
    Repeat(u8),
    StaleAfter(u8),
    ForeignAfter(u8),
}
#[derive(Clone, Copy, Debug)]
enum Anchor {
    End,
    At(u8),
    Selected,
    Stale,
    Foreign,
}
#[derive(Clone, Debug)]
enum Action {
    Insert(Vec<u64>, Anchor),
    Move(Select, Anchor),
    Remove(Select),
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    None,
    OldRevision,
    Commit,
}
#[derive(Clone, Debug)]
struct Step {
    action: Action,
    fault: Fault,
}

fn selection() -> impl Strategy<Value = Select> {
    prop_oneof![
        8 => vec(any::<u8>(), 0..=6).prop_map(Select::Ranks),
        3 => Just(Select::AllBackwards),
        1 => any::<u8>().prop_map(Select::Repeat),
        2 => any::<u8>().prop_map(Select::StaleAfter),
        1 => any::<u8>().prop_map(Select::ForeignAfter),
    ]
}
fn anchor() -> impl Strategy<Value = Anchor> {
    prop_oneof![3 => Just(Anchor::End), 6 => any::<u8>().prop_map(Anchor::At),
        2 => Just(Anchor::Selected), 1 => Just(Anchor::Stale), 1 => Just(Anchor::Foreign)]
}
fn steps() -> impl Strategy<Value = Vec<Step>> {
    let insert = (
        prop_oneof![9 => vec(1u64..=3, 1..=5),
        1 => Just(vec![1, 999]), 1 => Just(Vec::new())],
        anchor(),
    )
        .prop_map(|(tracks, anchor)| Action::Insert(tracks, anchor));
    let action = prop_oneof![6 => insert, 7 => (selection(), anchor()).prop_map(|(s, a)| Action::Move(s, a)),
        3 => selection().prop_map(Action::Remove)];
    vec(
        (
            action,
            prop_oneof![15 => Just(Fault::None), 2 => Just(Fault::OldRevision),
        2 => Just(Fault::Commit)],
        )
            .prop_map(|(action, fault)| Step { action, fault }),
        1..=40,
    )
}

// A handle is a position in the model's append-only allocation journal. It has
// no relationship to a playlist ID, queue ID or current ordinal.
type Handle = usize;
const FOREIGN: Handle = usize::MAX;
#[derive(Default)]
struct Model {
    order: Vec<Handle>,
    tracks: Vec<u64>,
    retired: Vec<Handle>,
    edits: u64,
    removed_track: Option<u64>,
}
impl Model {
    fn library_revision(&self) -> u64 {
        9 + u64::from(self.removed_track.is_some())
    }
    fn at(&self, rank: u8) -> Handle {
        if self.order.is_empty() {
            FOREIGN
        } else {
            self.order[rank as usize % self.order.len()]
        }
    }
    fn stale(&self) -> Handle {
        self.retired.last().copied().unwrap_or(FOREIGN)
    }
    fn select(&self, spec: &Select) -> Vec<Handle> {
        match spec {
            Select::Ranks(ranks) => {
                let mut out = Vec::new();
                for rank in ranks {
                    let handle = self.at(*rank);
                    if !out.contains(&handle) {
                        out.push(handle);
                    }
                }
                out
            }
            Select::AllBackwards => self.order.iter().rev().copied().collect(),
            Select::Repeat(rank) => vec![self.at(*rank); 2],
            Select::StaleAfter(rank) => vec![self.at(*rank), self.stale()],
            Select::ForeignAfter(rank) => vec![self.at(*rank), FOREIGN],
        }
    }
    fn anchor(&self, spec: Anchor, ids: &[Handle]) -> Option<Handle> {
        match spec {
            Anchor::End => None,
            Anchor::At(rank) => Some(self.at(rank)),
            Anchor::Selected => ids.first().copied().or_else(|| self.order.first().copied()),
            Anchor::Stale => Some(self.stale()),
            Anchor::Foreign => Some(FOREIGN),
        }
    }
}

#[derive(Default)]
struct Coverage(BTreeMap<&'static str, usize>);
impl Coverage {
    fn hit(&mut self, name: &'static str) {
        *self.0.entry(name).or_default() += 1;
    }
}

const TRACK_PATHS: [&str; 4] = ["", "/music/retained.wav", "/music/b.wav", "/music/c.wav"];
const QUEUE_FOREIGN: u64 = i64::MAX as u64;
struct Pair {
    store: Store,
    playlist: u64,
    other: u64,
    foreign: u64,
    // Per-handle identity bindings, including rows that have since disappeared.
    playlist_ids: Vec<u64>,
    queue_ids: Vec<u64>,
}
impl Pair {
    fn open(path: &Path) -> Result<Self> {
        let mut store = Store::open(path)?;
        seed_library(&store.connection)?;
        for (id, path) in [(2, TRACK_PATHS[2]), (3, TRACK_PATHS[3])] {
            store.connection.execute("INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES(?1,1,?2,1,1)", params![id, path])?;
            store.connection.execute("INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number)
                SELECT ?1,?1,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,?1 FROM library_tracks WHERE id=1", [id])?;
        }
        store.connection.execute(
            "UPDATE library_state SET track_count=3 WHERE singleton=1",
            [],
        )?;
        let create = |name: &str| PlaylistEdit::Create { name: name.into() };
        let other = store
            .edit_playlist(None, 1, create("other"))?
            .playlist
            .unwrap()
            .id;
        store.edit_playlist(
            Some(other),
            1,
            PlaylistEdit::Add {
                library_revision: 9,
                track_ids: vec![3],
                before: None,
            },
        )?;
        let foreign = store
            .playlist_entries(other, None, None, 0, 1, "")?
            .1
            .entries[0]
            .id;
        let playlist = store
            .edit_playlist(None, 3, create("subject"))?
            .playlist
            .unwrap()
            .id;
        Ok(Self {
            store,
            playlist,
            other,
            foreign,
            playlist_ids: Vec::new(),
            queue_ids: Vec::new(),
        })
    }
    fn playlist_id(&self, h: Handle) -> u64 {
        if h == FOREIGN {
            self.foreign
        } else {
            self.playlist_ids[h]
        }
    }
    fn queue_id(&self, h: Handle) -> u64 {
        if h == FOREIGN {
            QUEUE_FOREIGN
        } else {
            self.queue_ids[h]
        }
    }
    fn rows(&mut self, revision: u64, m: &Model) -> Result<Vec<(u64, u64)>> {
        let expected_library = m.library_revision();
        let (library, page) = self.store.playlist_entries(
            self.playlist,
            Some(revision),
            Some(expected_library),
            0,
            500,
            "",
        )?;
        ensure!(
            library == expected_library
                && page.revision == revision
                && page.total == page.entries.len() as u64
        );
        ensure!(
            page.entries
                .iter()
                .all(|e| e.available == (Some(e.track_id) != m.removed_track)),
            "availability of retained occurrence"
        );
        Ok(page
            .entries
            .into_iter()
            .map(|e| (e.id, e.track_id))
            .collect())
    }
    fn check(&mut self, model: &Model) -> Result<()> {
        let revision = 1 + model.edits;
        let p = self.rows(revision, model)?;
        let q: Vec<_> = self
            .store
            .queue_page(0, 500)?
            .into_iter()
            .map(|e| (e.id, e.track_id))
            .collect();
        let expected_p: Vec<_> = model
            .order
            .iter()
            .map(|&h| (self.playlist_ids[h], model.tracks[h]))
            .collect();
        let expected_q: Vec<_> = model
            .order
            .iter()
            .map(|&h| (self.queue_ids[h], Some(model.tracks[h])))
            .collect();
        ensure!(p == expected_p, "playlist rows {p:?} != {expected_p:?}");
        ensure!(q == expected_q, "queue rows {q:?} != {expected_q:?}");
        let load = self.store.load()?;
        ensure!(
            load.entries
                .iter()
                .map(|e| (e.id, e.path.as_path()))
                .eq(model.order.iter().map(|&h| {
                    (
                        self.queue_ids[h],
                        Path::new(TRACK_PATHS[model.tracks[h] as usize]),
                    )
                })),
            "queue restoration lost an occurrence or its playable path"
        );
        ensure!(
            (load.revision, load.next_id, load.playlists_revision)
                == (revision, 1 + model.tracks.len() as u64, 4 + model.edits),
            "revision/allocator leak"
        );
        let other = self
            .store
            .playlist_entries(
                self.other,
                Some(2),
                Some(model.library_revision()),
                0,
                500,
                "",
            )?
            .1;
        ensure!(
            other.total == 1
                && (other.entries[0].id, other.entries[0].track_id) == (self.foreign, 3)
                && other.entries[0].available == (model.removed_track != Some(3)),
            "edited another playlist"
        );
        Ok(())
    }
    fn run(&mut self, m: &mut Model, step: &Step, c: &mut Coverage) -> Result<()> {
        let (tracks, ids, before) = match &step.action {
            Action::Insert(tracks, anchor) => (tracks.as_slice(), vec![], m.anchor(*anchor, &[])),
            Action::Move(sel, anchor) => {
                let ids = m.select(sel);
                let a = m.anchor(*anchor, &ids);
                (&[][..], ids, a)
            }
            Action::Remove(sel) => (&[][..], m.select(sel), None),
        };
        let valid_ids = !ids.is_empty()
            && ids.iter().all(|h| m.order.contains(h))
            && ids.iter().collect::<HashSet<_>>().len() == ids.len();
        let valid_anchor = before.is_none_or(|h| m.order.contains(&h));
        let valid = valid_anchor
            && match step.action {
                Action::Insert(..) => {
                    !tracks.is_empty()
                        && tracks
                            .iter()
                            .all(|t| (1..=3).contains(t) && Some(*t) != m.removed_track)
                }
                _ => valid_ids,
            };
        let mut after = m.order.clone();
        if valid {
            match step.action {
                Action::Insert(..) => {
                    let at =
                        before.map_or(after.len(), |h| after.iter().position(|x| *x == h).unwrap());
                    after.splice(at..at, m.tracks.len()..m.tracks.len() + tracks.len());
                }
                Action::Remove(_) => after.retain(|h| !ids.contains(h)),
                Action::Move(..) if !before.is_some_and(|h| ids.contains(&h)) => {
                    // The request names a set, not the order in which to paste it.
                    let moving: Vec<_> =
                        after.iter().copied().filter(|h| ids.contains(h)).collect();
                    after.retain(|h| !ids.contains(h));
                    let at =
                        before.map_or(after.len(), |h| after.iter().position(|x| *x == h).unwrap());
                    after.splice(at..at, moving);
                }
                _ => (),
            }
        }
        let changed = after != m.order;
        let accept =
            valid && step.fault != Fault::OldRevision && !(changed && step.fault == Fault::Commit);
        let revision = 1 + m.edits - u64::from(step.fault == Fault::OldRevision);
        let pids: Vec<_> = ids.iter().map(|&h| self.playlist_id(h)).collect();
        let qids: Vec<_> = ids.iter().map(|&h| self.queue_id(h)).collect();
        let pb = before.map(|h| self.playlist_id(h));
        let qb = before.map(|h| self.queue_id(h));
        if step.fault == Fault::Commit {
            self.store.connection.execute_batch("CREATE TEMP TRIGGER p_abort AFTER UPDATE ON playlists_state BEGIN SELECT RAISE(ABORT,'late abort'); END;
                CREATE TEMP TRIGGER q_abort AFTER UPDATE OF revision ON control BEGIN SELECT RAISE(ABORT,'late abort'); END;")?;
        }
        let edit = match step.action {
            Action::Insert(..) => PlaylistEdit::Add {
                library_revision: m.library_revision(),
                track_ids: tracks.to_vec(),
                before: pb,
            },
            Action::Move(..) => PlaylistEdit::Move {
                entry_ids: pids,
                before: pb,
            },
            Action::Remove(_) => PlaylistEdit::Remove { entry_ids: pids },
        };
        let p = self
            .store
            .edit_playlist(Some(self.playlist), revision, edit);
        ensure!(
            p.is_ok() == accept,
            "unexpected playlist outcome: expected success {accept}, error {:?}",
            p.as_ref().err()
        );
        let first = 1 + m.tracks.len() as u64;
        let q = match step.action {
            Action::Insert(..) => Some(self.store.insert_tracks(
                revision,
                m.library_revision(),
                tracks.iter().enumerate().map(|(i, &t)| {
                    (
                        first + i as u64,
                        t,
                        Path::new(TRACK_PATHS.get(t as usize).unwrap_or(&"/missing")),
                    )
                }),
                qb,
            )),
            Action::Move(..) => Some(self.store.move_queue(revision, &qids, qb)),
            Action::Remove(_) if ids.is_empty() => None, // internal remove_entries has no minimum batch size
            Action::Remove(_) => Some(self.store.remove_entries(revision, &qids)),
        };
        if let Some(q) = q {
            ensure!(
                q.is_ok() == accept,
                "unexpected queue outcome: expected success {accept}, got {q:?}"
            );
        }
        if step.fault == Fault::Commit {
            self.store
                .connection
                .execute_batch("DROP TRIGGER p_abort; DROP TRIGGER q_abort;")?;
        }
        if ids
            .iter()
            .chain(before.iter())
            .any(|h| m.retired.contains(h))
        {
            c.hit("retired reference");
        }
        if accept {
            if changed {
                match step.action {
                    Action::Insert(..) => {
                        if tracks.iter().collect::<HashSet<_>>().len() < tracks.len() {
                            c.hit("duplicate tracks inserted");
                        }
                        c.hit(if before.is_some() {
                            "insert before"
                        } else {
                            "append"
                        });
                        // Bind only fresh opaque playlist IDs. Order among identical new
                        // tracks is indistinguishable until an ID is first observed. Do
                        // not overwrite the bindings for any existing/surviving row.
                        let rows = self.rows(2 + m.edits, m)?;
                        let fresh: Vec<_> = rows
                            .into_iter()
                            .filter(|(id, _)| !self.playlist_ids.contains(id))
                            .collect();
                        ensure!(
                            fresh.len() == tracks.len() && fresh.iter().map(|(_, t)| t).eq(tracks),
                            "inserted payload/order or identity reuse"
                        );
                        self.playlist_ids.extend(fresh.iter().map(|(id, _)| id));
                        self.queue_ids.extend(first..first + tracks.len() as u64);
                        m.tracks.extend_from_slice(tracks);
                    }
                    Action::Remove(_) => {
                        c.hit("remove");
                        m.retired.extend(&ids);
                    }
                    Action::Move(..) => {
                        c.hit("changed move");
                        if ids.windows(2).any(|w| {
                            m.order.iter().position(|h| h == &w[0])
                                > m.order.iter().position(|h| h == &w[1])
                        }) {
                            c.hit("changed move nonmonotonic request");
                        }
                    }
                }
                m.order = after;
                m.edits += 1;
            } else if before.is_some_and(|h| ids.contains(&h)) {
                c.hit("selected-anchor noop");
            } else {
                c.hit("same-order noop");
            }
            let response = p.unwrap();
            let list = response.playlist.unwrap();
            ensure!(
                response.revision == 4 + m.edits
                    && list.revision == 1 + m.edits
                    && list.entry_count == Some(m.order.len() as u64)
            );
        } else if valid && step.fault == Fault::Commit {
            c.hit("late write rollback");
        } else if step.fault == Fault::OldRevision {
            c.hit("stale revision");
        } else {
            c.hit("invalid operands");
        }
        if ids
            .iter()
            .any(|&h| h != FOREIGN && Some(m.tracks[h]) == m.removed_track)
            && changed
            && accept
        {
            c.hit("edited retained unavailable occurrence");
        }
        if !accept && tracks.iter().any(|&t| Some(t) == m.removed_track) {
            c.hit("cannot reinsert removed track");
        }
        let high_water = c.0.entry("max live").or_default();
        *high_water = (*high_water).max(m.order.len());
        self.check(m)
    }
    /// Remove through the real library worker while Store has the playlist and
    /// queue open. Request a track by two separate playlist occurrences.
    fn remove_track(
        &mut self,
        path: &Path,
        m: &mut Model,
        track: u64,
        c: &mut Coverage,
    ) -> Result<()> {
        use crate::library_types::{LibraryCommand, LibraryReply};
        use crate::listing::{Browser, Grouping, ListDescription, ListSource};
        let other = track % 3 + 1;
        let start = m.order.len();
        self.run(
            m,
            &Step {
                action: Action::Insert(vec![other, track, other, track], Anchor::End),
                fault: Fault::None,
            },
            c,
        )?;
        let rev = 1 + m.edits;
        let list = ListDescription {
            source: ListSource::Playlist {
                id: self.playlist,
                revision: rev,
            },
            browser: Browser::default(),
            query: String::new(),
            sort: vec![],
            group: Grouping::None,
        };
        let prepared = self.store.playlist_sources(self.playlist, rev, 9)?;
        let expected: Vec<_> = m
            .order
            .iter()
            .map(|&h| {
                let t = m.tracks[h];
                (t, PathBuf::from(TRACK_PATHS[t as usize]))
            })
            .collect();
        ensure!(prepared == expected);
        let request = TrackSelection::List {
            list: list.clone(),
            library_revision: 9,
            ranges: vec![[start as u64 + 1, u64::MAX]],
            excluded: vec![self.playlist_id(m.order[start + 2])],
            first_row_id: Some(self.playlist_id(m.order[start + 1])),
        };
        let worker = library::Library::start(path.to_owned())?;
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        let reply = runtime.block_on(
            worker
                .handle()
                .request(LibraryCommand::RemoveTracks { selection: request }),
        )?;
        ensure!(
            matches!(
                reply,
                LibraryReply::TracksRemoved {
                    removed: 1,
                    library_revision: 10
                }
            ),
            "removing two references to one track: {reply:?}"
        );
        ensure!(worker.state().tracks == 2);
        // Modern list pages and legacy playlist pages expose the same occurrences
        // even though only the former use the list compiler and its membership SQL.
        let reply = runtime.block_on(worker.handle().request(LibraryCommand::ListTracks {
            list: list.clone(),
            revision: Some(10),
            offset: 0,
            limit: 500,
        }))?;
        let LibraryReply::ListTracks { page, totals } = reply else {
            anyhow::bail!("expected list rows")
        };
        ensure!(page.revision == 10 && page.total == m.order.len() as u64);
        ensure!(totals.unwrap().tracks == page.total);
        ensure!(
            page.entries
                .iter()
                .map(|r| (r.row_id, r.track.id, r.available))
                .eq(m.order.iter().map(|&h| (
                    self.playlist_id(h),
                    m.tracks[h],
                    m.tracks[h] != track
                ))),
            "list pages lost unavailable occurrences, multiplicity, or order"
        );
        worker.shutdown()?;
        m.removed_track = Some(track);
        c.hit("library removed by duplicate occurrences");
        self.check(m)?;
        ensure!(
            self.store
                .playlist_entries(self.playlist, Some(rev), Some(9), 0, 500, "")
                .is_err()
        );
        ensure!(self.store.playlist_sources(self.playlist, rev, 10).is_err());

        // Replaying the prepared load must reject both its stale stamp and
        // its now-unavailable source even if the caller supplies a fresh stamp.
        for library_rev in [9, 10] {
            let first = 1 + m.tracks.len() as u64;
            let entries = prepared
                .iter()
                .enumerate()
                .map(|(i, (t, path))| (first + i as u64, *t, path.as_path()));
            ensure!(
                self.store
                    .enqueue_playlist(self.playlist, rev, library_rev, rev, entries, None)
                    .is_err()
            );
            self.check(m)?;
        }
        let mut excluded: Vec<_> = m
            .order
            .iter()
            .filter(|&&h| m.tracks[h] == track)
            .map(|&h| self.playlist_id(h))
            .collect();
        // Leave exactly one tombstone in the range. Exempting a different
        // occurrence with the same track must not exempt this one too.
        let last = excluded.pop().unwrap();
        let select = |excluded| TrackSelection::List {
            list: list.clone(),
            library_revision: 10,
            ranges: vec![[0, u64::MAX]],
            excluded,
            first_row_id: Some(self.playlist_id(m.order[0])),
        };
        let partial = select(excluded.clone());
        excluded.push(last);
        let available = select(excluded);
        ensure!(self.store.resolve_selection(&partial, 0).is_err());
        let survivors: Vec<_> = expected.into_iter().filter(|(t, _)| *t != track).collect();
        ensure!(self.store.resolve_selection(&available, 0)? == survivors);
        Ok(())
    }

    fn save_copy(&mut self, m: &Model, c: &mut Coverage) -> Result<()> {
        // Save Upcoming skips unavailable rows rather than rejecting the entire
        // list. Its input is a sequence of queue occurrence IDs in playback order.
        let ids: Vec<_> = m.order.iter().rev().map(|&h| self.queue_id(h)).collect();
        let expected: Vec<_> = m
            .order
            .iter()
            .rev()
            .map(|&h| m.tracks[h])
            .filter(|&t| Some(t) != m.removed_track)
            .collect();
        let catalog = 4 + m.edits;
        let copy = self.store.save_upcoming(1 + m.edits, catalog, "copy", &ids);
        if expected.is_empty() {
            ensure!(copy.is_err());
            ensure!(self.store.playlists_revision()? == catalog);
        } else {
            let copy = copy?;
            ensure!(copy.revision == catalog + 1);
            let p = copy.playlist.unwrap();
            let actual = self
                .store
                .playlist_entries(
                    p.id,
                    Some(p.revision),
                    Some(m.library_revision()),
                    0,
                    500,
                    "",
                )?
                .1;
            ensure!(actual.entries.iter().all(|e| e.available));
            ensure!(
                actual.entries.iter().map(|e| &e.track_id).eq(&expected),
                "saved queue membership/order differs"
            );
            if ids.len() > expected.len() {
                c.hit("save skipped unavailable among survivors");
            }
        }
        Ok(())
    }
}

fn run_history(
    initial: &[u64],
    steps: &[Step],
    removal: Option<(u8, u64)>,
    coverage: &mut Coverage,
) -> Result<()> {
    let scratch = Scratch::new();
    let path = scratch.database();
    let mut pair = Pair::open(&path)?;
    let mut model = Model::default();
    if !initial.is_empty() {
        pair.run(
            &mut model,
            &Step {
                action: Action::Insert(initial.to_vec(), Anchor::End),
                fault: Fault::None,
            },
            coverage,
        )?;
    }
    for (i, step) in steps.iter().enumerate() {
        if let Some((_, track)) = removal.filter(|(at, _)| usize::from(*at) % steps.len() == i) {
            pair.remove_track(&path, &mut model, track, coverage)?;
        }
        pair.run(&mut model, step, coverage)
            .with_context(|| format!("step {i}: {step:?}"))?;
    }
    drop(pair.store);
    pair.store = Store::open(&path)?;
    pair.check(&model)?;
    pair.save_copy(&model, coverage)
}

#[test]
fn occurrence_histories() {
    let coverage = RefCell::new(Coverage::default());
    let mut runner = TestRunner::new(Config {
        source_file: Some(file!()),
        test_name: Some("occurrence_histories"),
        ..Config::default()
    });
    let removal = prop::option::weighted(0.75, (any::<u8>(), 1u64..=3));
    runner
        .run(
            &(vec(1u64..=3, 0..=10), steps(), removal),
            |(initial, steps, removal)| {
                run_history(&initial, &steps, removal, &mut coverage.borrow_mut())
                    .map_err(|e| TestCaseError::fail(format!("{e:#}")))
            },
        )
        .unwrap();
    eprintln!("occurrence behavior counts: {:?}", coverage.into_inner().0);
}
