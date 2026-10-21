//! Observe saved stations through Store, with a schema-6 file as input.
use super::{
    stations::{StaleStationsRevision, StationChange, StationEdit},
    *,
};
use crate::protocol::{MAX_STATION_BATCH, MAX_STATIONS, Station};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("tyranny-stations-{}.sqlite", uuid::Uuid::new_v4())))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn spec(url: &str, title: &str) -> StreamSpec {
    StreamSpec::new(url.into(), title.into()).unwrap()
}
fn add(store: &mut Store, expected: u64, specs: &[(&str, &str)]) -> Result<StationChange> {
    store.edit_stations(
        expected,
        StationEdit::Add(specs.iter().map(|(url, name)| spec(url, name)).collect()),
    )
}
fn summary(stations: &[Station]) -> Vec<(&str, &str)> {
    stations
        .iter()
        .map(|station| (station.name.as_str(), station.url.as_str()))
        .collect()
}

#[test]
fn schema_seven_migrates_from_six_with_empty_catalog() -> Result<()> {
    let scratch = Scratch::new();
    let mut connection = Connection::open(&scratch.0)?;
    migrate(&mut connection, 6)?;
    connection.execute_batch(
        "INSERT INTO queue(id,ordinal,path,stream_title) VALUES(1,0,'/music/a.flac',NULL),
            (2,1,'http://radio.example/live','Live');
         UPDATE control SET next_id=3;",
    )?;
    drop(connection);
    let store = Store::open(&scratch.0)?;
    assert_eq!(store.stations()?, (0, vec![]));
    let loaded = store.load()?;
    assert_eq!(loaded.entries.len(), 2);
    assert_eq!(loaded.entries[1].stream.as_ref().unwrap().title, "Live");
    Ok(())
}

#[test]
fn add_update_remove_round_trip_and_survive_reopen() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.0)?;
    let added = add(
        &mut store,
        0,
        &[("http://a/x", "Alpha"), ("http://b/y", "Beta")],
    )?;
    assert_eq!(added.revision, 1);
    assert_eq!(
        summary(&added.stations),
        [("Alpha", "http://a/x"), ("Beta", "http://b/y")]
    );
    let (alpha, beta) = (added.stations[0].id, added.stations[1].id);
    let updated = store.edit_stations(
        1,
        StationEdit::Update {
            id: alpha,
            spec: spec("http://c/z", "Gamma"),
        },
    )?;
    assert_eq!(updated.revision, 2);
    assert_eq!(
        updated.stations,
        [Station {
            id: alpha,
            name: "Gamma".into(),
            url: "http://c/z".into()
        }]
    );
    let removed = store.edit_stations(2, StationEdit::Remove { id: beta })?;
    assert_eq!((removed.revision, removed.stations.len()), (3, 0));
    drop(store);
    let (revision, stations) = Store::open(&scratch.0)?.stations()?;
    assert_eq!(revision, 3);
    assert_eq!(summary(&stations), [("Gamma", "http://c/z")]);
    assert_eq!(stations[0].id, alpha);
    Ok(())
}

#[test]
fn add_skips_urls_already_saved() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.0)?;
    add(&mut store, 0, &[("http://a/x", "A")])?;
    let second = add(
        &mut store,
        1,
        &[("http://A/x", "Again"), ("http://b/y", "B")],
    )?;
    assert_eq!(summary(&second.stations), [("B", "http://b/y")]);
    assert_eq!(second.revision, 2);
    let (revision, stations) = store.stations()?;
    assert_eq!((revision, stations.len()), (2, 2));
    let skipped = add(&mut store, 2, &[("http://a/x", "A")])?;
    assert_eq!((skipped.revision, skipped.stations.len()), (3, 0));
    Ok(())
}

#[test]
fn stations_sort_by_name_case_insensitively() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.0)?;
    add(
        &mut store,
        0,
        &[("http://1/", "b"), ("http://2/", "A"), ("http://3/", "c")],
    )?;
    let names: Vec<_> = store
        .stations()?
        .1
        .into_iter()
        .map(|station| station.name)
        .collect();
    assert_eq!(names, ["A", "b", "c"]);
    Ok(())
}

#[test]
fn update_to_an_existing_url_is_invalid_and_commits_nothing() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.0)?;
    let added = add(&mut store, 0, &[("http://a/x", "A"), ("http://b/y", "B")])?;
    let before = store.stations()?;
    let error = store
        .edit_stations(
            1,
            StationEdit::Update {
                id: added.stations[0].id,
                spec: spec("http://b/y", "Renamed"),
            },
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<PlaylistFailure>().unwrap().code,
        ErrorCode::InvalidCommand
    );
    assert_eq!(store.stations()?, before);
    // Keeping its own URL while renaming is fine.
    store.edit_stations(
        1,
        StationEdit::Update {
            id: added.stations[0].id,
            spec: spec("http://a/x", "Renamed"),
        },
    )?;
    Ok(())
}

#[test]
fn unknown_ids_are_invalid() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.0)?;
    for edit in [
        StationEdit::Remove { id: 9 },
        StationEdit::Update {
            id: 9,
            spec: spec("http://a/x", "A"),
        },
    ] {
        let error = store.edit_stations(0, edit).unwrap_err();
        assert!(error.is::<PlaylistFailure>());
    }
    assert_eq!(store.stations()?, (0, vec![]));
    Ok(())
}

#[test]
fn stale_revision_and_limits_commit_nothing() -> Result<()> {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.0)?;
    let specs = |range: std::ops::Range<usize>| -> Vec<StreamSpec> {
        range.map(|n| spec(&format!("http://h/{n}"), "S")).collect()
    };
    store.edit_stations(0, StationEdit::Add(specs(0..MAX_STATION_BATCH)))?;
    store.edit_stations(1, StationEdit::Add(specs(MAX_STATION_BATCH..MAX_STATIONS)))?;
    let full = store.stations()?;
    assert_eq!((full.0, full.1.len()), (2, MAX_STATIONS));

    let stale = store
        .edit_stations(1, StationEdit::Remove { id: full.1[0].id })
        .unwrap_err();
    assert!(stale.is::<StaleStationsRevision>());
    assert!(!stale.is::<PlaylistFailure>());
    let over_catalog = store
        .edit_stations(2, StationEdit::Add(specs(MAX_STATIONS..MAX_STATIONS + 1)))
        .unwrap_err();
    assert!(over_catalog.is::<PlaylistFailure>());
    assert_eq!(store.stations()?, full);

    let mut other = Store::open(&Scratch::new().0)?;
    let over_batch = other
        .edit_stations(0, StationEdit::Add(specs(0..MAX_STATION_BATCH + 1)))
        .unwrap_err();
    assert!(over_batch.is::<PlaylistFailure>());
    assert_eq!(other.stations()?, (0, vec![]));
    let empty = other
        .edit_stations(0, StationEdit::Add(vec![]))
        .unwrap_err();
    assert!(empty.is::<PlaylistFailure>());
    Ok(())
}
