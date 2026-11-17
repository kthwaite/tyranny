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
