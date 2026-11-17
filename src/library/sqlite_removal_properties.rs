use super::*;
use crate::protocol::TrackSelection;
use proptest::prelude::*;

proptest! {
    #[test]
    fn ranges_remove_exactly_the_displayed_tracks(
        // Yesterday always survives the Recent cutoff. Stay clear of the rolling
        // second at its boundary: the two requests legitimately have separate clocks.
        ages in prop::collection::vec(prop_oneof![Just(1i64), Just(29), Just(31), Just(-1)], 0..8),
        recent in any::<bool>(), descending in any::<bool>(), anchor in any::<bool>(),
        pick in prop::collection::vec(any::<bool>(), 8), start in 0..8usize,
    ) {
        let now = crate::listing::unix_now();
        let ages: Vec<_> = [1].into_iter().chain(ages).collect();
        let tracks: Vec<_> = ages.iter().enumerate().map(|(i,age)|
            track(i as u64+1, 1, 1, "Song", (1, i as u32+1), None,
                ("FLAC", None, 44100, None), (now-age*86400,None,None))).collect();
        let mut db = connection(&tracks);
        crate::store::migrate(&mut db, 11).unwrap();
        db.execute("UPDATE library_state SET track_count=?1", [tracks.len()]).unwrap();
        let mut list = list(if recent {ListSource::Recent} else {ListSource::Library});
        list.sort.push(SortKey { field: SortField::AlbumOrder, descending });
        let LibraryReply::ListTracks { page, .. } = lists::execute(&mut db, LibraryCommand::ListTracks {
            list: list.clone(), revision: Some(11), offset: 0, limit: 10,
        }, now).unwrap() else {unreachable!()};
        let ids: Vec<_> = page.entries.iter().map(|r| r.row_id).collect();
        // Check membership independently before using displayed positions as the order oracle.
        let mut members = ids.clone();
        members.sort_unstable();
        prop_assert_eq!(members, ages.iter().enumerate().filter(|(_,a)| !recent || **a <= 30)
            .map(|(i,_)| i as u64+1).collect::<Vec<_>>());
        let start = start % ids.len();
        let excluded: Vec<_> = ids[start+1..].iter().zip(&pick).filter(|(_,skip)| **skip).map(|(&id,_)|id).collect();
        let expected: Vec<_> = ids[start..].iter().filter(|id| !excluded.contains(id)).copied().collect();
        let removed = db::remove_tracks(&mut db, &TrackSelection::List {
            list, library_revision: 11, ranges: vec![[start as u64, ids.len() as u64]],
            first_row_id: anchor.then_some(ids[start]), excluded,
        });
        prop_assert!(removed.is_ok(), "displayed {ids:?}, expected {expected:?}: {removed:?}");
        prop_assert_eq!(removed.unwrap(), (expected.len(),12));
        let actual = db.prepare("SELECT id FROM library_assets WHERE excluded ORDER BY id").unwrap()
            .query_map([], |r|r.get::<_,u64>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        let mut expected = expected;
        expected.sort_unstable();
        prop_assert_eq!(actual, expected);
    }
}

// Metamorphic cross-check: each kind of list exposes the same range/occurrence
// contract to destructive edits. Tombstones in regular playlists are selectable
// but must either be excluded or reject the entire edit.
#[test]
fn sqlite_removal_matches_display_for_every_source() {
    use std::collections::BTreeSet;
    for source in [
        ListSource::Library, ListSource::Album {id:1}, ListSource::Artist {id:1},
        ListSource::AppearsOn {artist_id:4}, ListSource::Compilations,
        ListSource::Genre {id:2}, ListSource::Recent,
        ListSource::Playlist {id:1,revision:1}, ListSource::Smart {id:2,revision:1},
    ] {
        for fragmented in [false,true] {
            for skip_missing in [false,true] {
                let mut db = lists_fixture::library();
                crate::store::migrate(&mut db,11).unwrap();
                db.execute_batch("UPDATE library_state SET track_count=8;
                    INSERT INTO playlists(id,name,search,revision,query) VALUES
                    (1,'Manual','manual',1,NULL),(2,'Rule','rule',1,'year:1970');
                    INSERT INTO playlist_entries(id,playlist_id,track_id,ordinal) VALUES
                    (101,1,2,1),(102,1,1,2),(103,1,9,3),(104,1,2,4)").unwrap();
                let now = crate::listing::unix_now();
                db.execute("UPDATE library_tracks SET date_added=?1 WHERE id IN (1,2)", [now-86_400]).unwrap();
                let list = list(source.clone());
                let LibraryReply::ListTracks {page,..} = lists::execute(&mut db,
                    LibraryCommand::ListTracks {list:list.clone(),revision:Some(11),offset:0,limit:32},now
                ).unwrap() else { unreachable!() };
                assert!(!page.entries.is_empty(), "{source:?}");
                let ranges = if fragmented {
                    (0..page.entries.len() as u64).step_by(2).map(|i|[i,i+1]).collect()
                } else { vec![[0,u64::MAX]] };
                let excluded: Vec<_> = page.entries.iter().filter(|r|skip_missing && !r.available)
                    .map(|r|r.row_id).collect();
                let selected: Vec<_> = page.entries.iter().enumerate().filter(|(i,r)|
                    (!fragmented || i%2==0) && !excluded.contains(&r.row_id)).map(|(_,r)|r).collect();
                let valid = selected.iter().all(|r| r.available);
                let ids: BTreeSet<_> = selected.iter().filter(|_|valid).map(|r|r.track.id).collect();
                let result = db::remove_tracks(&mut db,&TrackSelection::List {
                    list, library_revision:11,ranges,excluded,first_row_id:Some(page.entries[0].row_id)
                });
                if valid { assert_eq!(result.unwrap(),(ids.len(),12)); }
                else { assert_eq!(classify(result.unwrap_err()).kind,LibraryErrorKind::Invalid); }
                let actual = db.prepare("SELECT id FROM library_assets WHERE excluded").unwrap()
                    .query_map([], |row|row.get::<_,u64>(0)).unwrap().collect::<rusqlite::Result<BTreeSet<_>>>().unwrap();
                assert_eq!(actual,ids,"{source:?}");
                let counters: (usize,u64) = db.query_row("SELECT track_count,revision FROM library_state",[],
                    |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
                assert_eq!(counters,(8-ids.len(),11+u64::from(valid)));
            }
        }
    }
}
