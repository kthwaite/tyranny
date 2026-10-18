//! Exercise the Library request/state seam with actual native metadata probes.
//! The small WAV fixture is self-contained; tagged FLAC fixtures use the existing
//! developer FFmpeg executable only to create inputs, never to perform indexing.
use super::*;
use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("tyranny-library-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn library(&self) -> Library {
        let database = self.path("library.sqlite");
        if !database.exists() {
            let mut connection = Connection::open(&database).unwrap();
            let transaction = connection.transaction().unwrap();
            migrate(&transaction).unwrap();
            migrate_v5(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        Library::start(database).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wav(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let frames = 48u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&192_000u32.to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 4).to_le_bytes());
    bytes.resize(bytes.len() + frames as usize * 4, 0);
    fs::write(path, bytes).unwrap();
}
fn flac(path: &Path, tags: &[(&str, &str)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=48000:cl=stereo",
        "-t",
        "0.01",
        "-c:a",
        "flac",
    ]);
    for (key, value) in tags {
        command.arg("-metadata").arg(format!("{key}={value}"));
    }
    let output = command
        .arg(path)
        .output()
        .expect("FFmpeg is required to create tagged test media");
    assert!(
        output.status.success(),
        "fixture FFmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn page() -> PageRequest {
    PageRequest {
        revision: None,
        offset: 0,
        limit: 500,
        query: String::new(),
    }
}
async fn add(library: &Library, path: &Path) -> u64 {
    match library
        .handle()
        .request(LibraryCommand::AddRoot {
            path: path.to_str().unwrap().into(),
        })
        .await
        .unwrap()
    {
        LibraryReply::RootAdded(root) => root.id,
        other => panic!("unexpected add reply: {other:?}"),
    }
}
async fn finished(library: &Library) -> ScanStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let scan = library.state().scan;
        if !matches!(scan.phase, ScanPhase::Discovering | ScanPhase::Indexing) {
            return scan;
        }
        assert!(Instant::now() < deadline, "scan did not finish: {scan:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
async fn rescan(library: &Library, root_id: u64) -> ScanStatus {
    library
        .handle()
        .request(LibraryCommand::Rescan { root_id })
        .await
        .unwrap();
    finished(library).await
}
async fn tracks(library: &Library) -> Page<Track> {
    match library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: page(),
        })
        .await
        .unwrap()
    {
        LibraryReply::Tracks(page) => page,
        other => panic!("unexpected tracks reply: {other:?}"),
    }
}
async fn albums(library: &Library) -> Page<Album> {
    match library
        .handle()
        .request(LibraryCommand::Albums {
            artist_id: None,
            page: page(),
        })
        .await
        .unwrap()
    {
        LibraryReply::Albums(page) => page,
        other => panic!("unexpected albums reply: {other:?}"),
    }
}

#[tokio::test]
async fn imports_persist_without_rescan_and_unchanged_files_skip_probing() {
    let scratch = Scratch::new();
    let source = scratch.path("music/名字.wav");
    wav(&source);
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    let scan = finished(&library).await;
    assert_eq!(
        (
            scan.phase,
            scan.discovered,
            scan.processed,
            scan.updated,
            scan.failures
        ),
        (ScanPhase::Complete, 1, 1, 1, 0)
    );
    let imported = tracks(&library).await.entries.remove(0);
    assert_eq!(imported.metadata.title, "名字");
    assert_eq!(imported.metadata.artist, "Unknown Artist");
    assert_eq!(imported.metadata.album, "Unknown Album");
    assert_eq!(imported.metadata.album_artist, "Unknown Artist");
    assert_eq!(
        (imported.metadata.sample_rate, imported.metadata.channels),
        (48_000, 2)
    );
    let revision = library.state().revision;
    library.shutdown().unwrap();
    let library = scratch.library();
    assert_eq!(library.state().revision, revision);
    assert_eq!(library.state().scan.phase, ScanPhase::Complete);
    let reopened = tracks(&library).await.entries.remove(0);
    assert_eq!(reopened, imported);
    let scan = rescan(&library, root).await;
    assert_eq!((scan.updated, scan.unchanged, scan.processed), (0, 1, 1));
    assert_eq!(tracks(&library).await.entries[0], imported);
    let connection = Connection::open(scratch.path("library.sqlite")).unwrap();
    assert_eq!(
        resolve_track(&connection, imported.id, library.state().revision).unwrap(),
        source.to_str().unwrap()
    );
    // An unchanged rescan only moves scan status, so the revision holds.
    assert_eq!(library.state().revision, revision);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn unavailable_roots_preserve_tracks_and_complete_rescans_tombstone_without_recycling_ids() {
    let scratch = Scratch::new();
    let source = scratch.path("music/song.wav");
    wav(&source);
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let imported = tracks(&library).await.entries.remove(0);
    fs::rename(scratch.path("music"), scratch.path("unmounted")).unwrap();
    let scan = rescan(&library, root).await;
    assert_eq!(scan.phase, ScanPhase::Failed);
    assert!(scan.failures > 0 && scan.last_error.is_some());
    assert_eq!(tracks(&library).await.entries[0], imported);
    assert_eq!(library.state().tracks, 1);
    fs::rename(scratch.path("unmounted"), scratch.path("music")).unwrap();
    fs::remove_file(&source).unwrap();
    assert_eq!(rescan(&library, root).await.phase, ScanPhase::Complete);
    assert!(tracks(&library).await.entries.is_empty());
    assert!(albums(&library).await.entries.is_empty());
    assert_eq!(library.state().tracks, 0);
    let connection = Connection::open(scratch.path("library.sqlite")).unwrap();
    let unavailable =
        resolve_track(&connection, imported.id, library.state().revision).unwrap_err();
    assert_eq!(
        unavailable.downcast_ref::<LibraryError>().unwrap().kind,
        LibraryErrorKind::Invalid
    );
    wav(&source);
    assert_eq!(rescan(&library, root).await.phase, ScanPhase::Complete);
    let restored = tracks(&library).await.entries.remove(0);
    assert_eq!(
        (restored.id, restored.asset_id, restored.album_id),
        (imported.id, imported.asset_id, imported.album_id)
    );
    assert_eq!(library.state().tracks, 1);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn corrupt_changed_files_keep_metadata_and_are_retried_while_missing_files_reconcile() {
    let scratch = Scratch::new();
    let changed = scratch.path("music/changed.wav");
    let missing = scratch.path("music/missing.wav");
    wav(&changed);
    wav(&missing);
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let original = tracks(&library)
        .await
        .entries
        .into_iter()
        .find(|track| track.path == changed.to_str().unwrap())
        .unwrap();
    fs::write(&changed, b"not an audio container").unwrap();
    fs::remove_file(missing).unwrap();
    for _ in 0..2 {
        let scan = rescan(&library, root).await;
        assert_eq!(
            (scan.phase, scan.failures, scan.processed, scan.unchanged),
            (ScanPhase::Complete, 1, 1, 0)
        );
        let page = tracks(&library).await;
        assert_eq!(page.total, 1);
        assert_eq!(
            (page.entries[0].id, &page.entries[0].metadata),
            (original.id, &original.metadata)
        );
        assert!(page.entries[0].error.is_some());
    }
    wav(&changed);
    let scan = rescan(&library, root).await;
    assert_eq!((scan.updated, scan.failures), (1, 0));
    assert!(tracks(&library).await.entries[0].error.is_none());
    library.shutdown().unwrap();
}

#[tokio::test]
async fn album_identity_respects_compilations_release_ids_discs_and_separate_folders() {
    let scratch = Scratch::new();
    for folder in ["first", "second"] {
        flac(
            &scratch.path(&format!("music/{folder}/song.flac")),
            &[
                ("title", "Été %_"),
                ("artist", "ÅRTIST"),
                ("album_artist", "Owner"),
                ("album", "Duplicate"),
            ],
        );
    }
    for (disc, artist) in [("1/2", "Singer One"), ("2/2", "Singer Two")] {
        flac(
            &scratch.path(&format!("music/disc-{}/song.flac", &disc[..1])),
            &[
                ("title", "Compilation song"),
                ("artist", artist),
                ("album", "Shared release"),
                ("compilation", "1"),
                ("disc", disc),
                ("track", "1/12"),
                ("MUSICBRAINZ_ALBUMID", "release-identity"),
                ("MUSICBRAINZ_RELEASEGROUPID", "release-group"),
                ("REPLAYGAIN_TRACK_GAIN", "-4.50 dB"),
            ],
        );
    }
    let library = scratch.library();
    add(&library, &scratch.path("music")).await;
    assert_eq!(finished(&library).await.updated, 4);
    let page = albums(&library).await;
    assert_eq!(page.total, 3);
    assert_eq!(
        page.entries
            .iter()
            .filter(|album| album.title == "Duplicate")
            .count(),
        2
    );
    let compilation = page
        .entries
        .iter()
        .find(|album| album.title == "Shared release")
        .unwrap();
    assert_eq!((compilation.track_count, compilation.disc_count), (2, 2));
    assert!(compilation.compilation);
    assert_eq!(compilation.album_artist, "Various Artists");
    let LibraryReply::Tracks(compilation_tracks) = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: Some(compilation.id),
            page: page_request(None, 0, 500, ""),
        })
        .await
        .unwrap()
    else {
        panic!("wrong tracks reply");
    };
    assert_eq!(
        compilation_tracks
            .entries
            .iter()
            .map(|track| track.metadata.artist.as_str())
            .collect::<Vec<_>>(),
        ["Singer One", "Singer Two"]
    );
    assert_eq!(compilation_tracks.entries[0].metadata.track_total, Some(12));
    assert_eq!(
        compilation_tracks.entries[0].metadata.track_gain_db,
        Some(-4.5)
    );
    assert_eq!(
        compilation_tracks.entries[0]
            .metadata
            .musicbrainz_release_group_id
            .as_deref(),
        Some("release-group")
    );
    let LibraryReply::Artists(artists) = library
        .handle()
        .request(LibraryCommand::Artists(page_request(None, 0, 500, "")))
        .await
        .unwrap()
    else {
        panic!("wrong artist reply");
    };
    assert_eq!(artists.total, 2);
    assert_eq!(
        artists
            .entries
            .iter()
            .find(|artist| artist.name == "Owner")
            .unwrap()
            .album_count,
        2
    );
    library.shutdown().unwrap();
}

fn page_request(revision: Option<u64>, offset: usize, limit: usize, query: &str) -> PageRequest {
    PageRequest {
        revision,
        offset,
        limit,
        query: query.into(),
    }
}

#[test]
fn ordered_track_pages_keep_path_ties_filters_and_availability() {
    // Explicit metadata-only fixture: query ordering, not native import coverage.
    // IDs deliberately disagree with paths within one indexed ordering prefix.
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    migrate(&transaction).unwrap();
    transaction
        .execute_batch(
            "INSERT INTO library_roots(id,path,search,completed_scan)
             VALUES(1,'/library','/library',5);
             INSERT INTO library_artists(id,identity,name,search)
             VALUES(1,'artist','Artist','artist');
             INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
             VALUES(1,'first',1,'First','Artist','first','first',0),
                   (2,'second',1,'Second','Artist','second','second',0);
             UPDATE library_state SET revision=17,root_count=1,track_count=6;",
        )
        .unwrap();
    for (id, path, title, disc, track, album, indexed, seen) in [
        (1, "z.wav", "Same", 1, 1, 1, 1, 5),
        (2, "a.wav", "Same", 1, 1, 1, 1, 5),
        (3, "m.wav", "Same", 1, 1, 2, 1, 5),
        (4, "before.wav", "Before", 1, 1, 1, 1, 5),
        (5, "unindexed.wav", "Same", 1, 1, 1, 0, 5),
        (6, "tombstoned.wav", "Same", 1, 1, 1, 1, 4),
        (7, "track-two.wav", "After", 1, 2, 1, 1, 5),
        (8, "disc-two.wav", "Disc 2", 2, 1, 1, 1, 5),
    ] {
        let metadata = TrackMetadata {
            title: title.into(),
            disc_number: Some(disc),
            track_number: Some(track),
            ..TrackMetadata::default()
        };
        transaction
            .execute(
                "INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan,error)
                 VALUES(?1,1,?2,?3,?4,?5)",
                params![
                    id,
                    format!("/library/{path}"),
                    indexed,
                    seen,
                    (id == 1).then_some("retained probe error")
                ],
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,
                 disc_number,disc_total,track_number) VALUES(?1,?1,1,?2,?3,?4,?4,?5,2,?6)",
                params![
                    id,
                    album,
                    serde_json::to_string(&metadata).unwrap(),
                    title.to_lowercase(),
                    disc,
                    track
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    for (offset, album_id, query, total, expected) in [
        (0, None, "", 6, vec![4, 2]),
        (2, None, "", 6, vec![3, 1]),
        (4, None, "", 6, vec![7, 8]),
        (6, None, "", 6, vec![]),
        (1, None, "SAME", 3, vec![3, 1]),
        (1, Some(1), "same", 2, vec![1]),
    ] {
        let LibraryReply::Tracks(result) = query::execute(
            &mut connection,
            LibraryCommand::Tracks {
                album_id,
                page: page_request(Some(17), offset, 2, query),
            },
        )
        .unwrap() else {
            panic!("wrong tracks reply");
        };
        assert_eq!(
            (result.revision, result.total, result.offset),
            (17, total, offset)
        );
        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            expected
        );
        if let Some(retained) = result.entries.iter().find(|entry| entry.id == 1) {
            assert_eq!(retained.error.as_deref(), Some("retained probe error"));
        }
    }
    let stale = query::execute(
        &mut connection,
        LibraryCommand::Tracks {
            album_id: None,
            page: page_request(Some(16), 2, 2, ""),
        },
    )
    .unwrap_err();
    assert_eq!(
        stale
            .downcast_ref::<StaleLibraryRevision>()
            .unwrap()
            .current,
        17
    );
}

#[tokio::test]
async fn unicode_literal_filters_pages_and_revisions_are_consistent() {
    let scratch = Scratch::new();
    wav(&scratch.path("music/ÉTÉ %_.wav"));
    wav(&scratch.path("music/été second.wav"));
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let LibraryReply::Tracks(first) = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: page_request(None, 0, 1, "Été"),
        })
        .await
        .unwrap()
    else {
        panic!("wrong tracks reply");
    };
    assert_eq!((first.total, first.entries.len()), (2, 1));
    let LibraryReply::Tracks(second) = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: page_request(Some(first.revision), 1, 1, "ÉTÉ"),
        })
        .await
        .unwrap()
    else {
        panic!("wrong tracks reply");
    };
    assert_eq!(second.total, 2);
    assert_ne!(first.entries[0].id, second.entries[0].id);
    let LibraryReply::Tracks(literal) = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: page_request(None, 0, 500, "%_"),
        })
        .await
        .unwrap()
    else {
        panic!("wrong tracks reply");
    };
    assert_eq!(literal.total, 1);
    assert_eq!(literal.entries[0].metadata.title, "ÉTÉ %_");
    // A changed file installs a track, which moves the content revision.
    let file = fs::OpenOptions::new()
        .write(true)
        .open(scratch.path("music/été second.wav"))
        .unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(file);
    rescan(&library, root).await;
    let stale = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: page_request(Some(first.revision), 0, 1, ""),
        })
        .await
        .unwrap_err();
    assert_eq!(stale.kind, LibraryErrorKind::StaleRevision);
    for limit in [0, 501] {
        let failure = library
            .handle()
            .request(LibraryCommand::Roots(page_request(None, 0, limit, "")))
            .await
            .unwrap_err();
        assert_eq!(failure.kind, LibraryErrorKind::Invalid);
    }
    let failure = library
        .handle()
        .request(LibraryCommand::Artists(page_request(
            None,
            0,
            1,
            &"x".repeat(1025),
        )))
        .await
        .unwrap_err();
    assert_eq!(failure.kind, LibraryErrorKind::InvalidFilter);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn root_registration_rejects_overlaps_and_unknown_roots() {
    let scratch = Scratch::new();
    fs::create_dir_all(scratch.path("music/subdirectory")).unwrap();
    let library = scratch.library();
    add(&library, &scratch.path("music")).await;
    finished(&library).await;
    for path in [
        scratch.path("music"),
        scratch.path("music/subdirectory"),
        scratch.0.clone(),
        PathBuf::from("relative"),
    ] {
        let failure = library
            .handle()
            .request(LibraryCommand::AddRoot {
                path: path.to_str().unwrap().into(),
            })
            .await
            .unwrap_err();
        assert_eq!(failure.kind, LibraryErrorKind::Invalid);
    }
    assert_eq!(
        library
            .handle()
            .request(LibraryCommand::Rescan { root_id: 99 })
            .await
            .unwrap_err()
            .kind,
        LibraryErrorKind::Invalid
    );
    assert_eq!(library.state().roots, 1);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn interruption_keeps_committed_index_and_queries_remain_responsive_during_scan() {
    let scratch = Scratch::new();
    wav(&scratch.path("music/old.wav"));
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let original = tracks(&library).await.entries.remove(0);
    for index in 0..1024 {
        wav(&scratch.path(&format!("music/new/{index}.wav")));
    }
    library
        .handle()
        .request(LibraryCommand::Rescan { root_id: root })
        .await
        .unwrap();
    let LibraryReply::Tracks(during) = tokio::time::timeout(
        Duration::from_secs(2),
        library.handle().request(LibraryCommand::Tracks {
            album_id: None,
            page: page_request(None, 0, 1, "old"),
        }),
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("wrong tracks reply");
    };
    assert_eq!(during.entries[0].id, original.id);
    assert_eq!(
        library
            .handle()
            .request(LibraryCommand::Rescan { root_id: root })
            .await
            .unwrap_err()
            .kind,
        LibraryErrorKind::Busy
    );
    library.shutdown().unwrap();
    let library = scratch.library();
    assert_eq!(library.state().scan.phase, ScanPhase::Interrupted);
    let LibraryReply::Tracks(reopened) = library
        .handle()
        .request(LibraryCommand::Tracks {
            album_id: None,
            page: page_request(None, 0, 1, "old"),
        })
        .await
        .unwrap()
    else {
        panic!("wrong tracks reply");
    };
    assert_eq!(reopened.entries[0].id, original.id);
    assert_eq!(library.state().roots, 1);
    library.shutdown().unwrap();
}

fn structured_filter_fixture() -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    migrate(&transaction).unwrap();
    transaction.execute_batch(
        "INSERT INTO library_roots(id,path,search,completed_scan) VALUES(1,'/music','/music',1);
         INSERT INTO library_artists(id,identity,name,search) VALUES
         (1,'miles','Miles Davis','miles davis'),(2,'ornette','Ornette Coleman','ornette coleman'),
         (3,'various','Various Artists','various artists');
         INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation)
         VALUES(1,'blue',1,'Kind of Blue','Miles Davis','kind of blue miles davis','kind of blue',0),
         (2,'shapes',2,'Shapes','Ornette Coleman','shapes ornette coleman','shapes',0),
         (3,'mix',3,'Collection','Various Artists','collection various artists','collection',1);
         UPDATE library_state SET revision=7;"
    ).unwrap();
    for (id, title, artist, album, available) in [
        (1, "So What", 1, 1, true),
        (2, "Lonely Woman", 2, 2, true),
        (3, "ÉTÉ %_ \"Live\" \\ ' OR 1=1 --", 1, 3, true),
        (4, "Peace", 2, 3, true),
        (5, "Hidden", 1, 3, false),
        (6, "and or not", 3, 3, true),
    ] {
        transaction.execute(
            "INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan) VALUES(?1,1,?2,?3,1)",
            params![id, format!("/music/{id}.wav"), available],
        ).unwrap();
        let artist_name = match artist {
            1 => "Miles Davis",
            2 => "Ornette Coleman",
            _ => "Various Artists",
        };
        let (album_title, album_artist) = match album {
            1 => ("Kind of Blue", "Miles Davis"),
            2 => ("Shapes", "Ornette Coleman"),
            _ => ("Collection", "Various Artists"),
        };
        let metadata = TrackMetadata {
            title: title.into(),
            artist: artist_name.into(),
            album: album_title.into(),
            album_artist: album_artist.into(),
            ..Default::default()
        };
        transaction.execute(
            "INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,
             disc_number,disc_total,track_number) VALUES(?1,?1,?2,?3,?4,?5,?6,1,1,?1)",
            params![id, artist, album, serde_json::to_string(&metadata).unwrap(),
                format!("{title}\n{artist_name}\n{album_title}\n{album_artist}\n/music/{id}.wav").to_lowercase(),
                title.to_lowercase()],
        ).unwrap();
    }
    transaction.commit().unwrap();
    connection
}

#[test]
fn structured_filters_return_ordered_ids_for_operators_lists_and_literals() {
    let mut connection = structured_filter_fixture();
    for (filter, expected) in [
        ("", vec![1, 2, 3, 4, 6]),
        (" \t\n ", vec![1, 2, 3, 4, 6]),
        (r#"artist:"Miles Davis" & !album:"Kind of Blue""#, vec![3]),
        (
            r#"artist:["Miles Davis", "Ornette Coleman"]"#,
            vec![1, 2, 3, 4],
        ),
        (r#"!artist:["Miles Davis", "Ornette Coleman"]"#, vec![6]),
        ("title:Peace | title:What & artist:Ornette", vec![4]),
        ("(title:Peace | title:What) & artist:Ornette", vec![4]),
        ("title:What OR title:Peace AND artist:Ornette", vec![1, 4]),
        ("(title:What OR title:Peace) AND artist:Ornette", vec![4]),
        ("NOT NOT title:What", vec![1]),
        ("artist:Miles album:Collection", vec![3]),
        ("Miles Collection", vec![3]),
        ("Various", vec![3, 4, 6]),
        ("/music/3.wav", vec![3]),
        ("Collection Miles", vec![3]),
        (r#""Collection Miles""#, vec![]),
        (r#""Lonely Woman""#, vec![2]),
        (r#"title:"Été %_ \"Live\" \\""#, vec![3]),
        (r#"title:"' OR 1=1 --""#, vec![3]),
        ("%_", vec![3]),
        ("and or not", vec![6]),
        ("artist:Various", vec![6]),
        ("album:Various", vec![]),
        ("title:Hidden", vec![]),
    ] {
        let LibraryReply::Tracks(page) = query::execute(
            &mut connection,
            LibraryCommand::Tracks {
                album_id: None,
                page: page_request(Some(7), 0, 500, filter),
            },
        )
        .unwrap() else {
            panic!("expected tracks");
        };
        assert_eq!(
            page.entries
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            expected,
            "{filter}"
        );
        assert_eq!(page.total, expected.len() as u64, "{filter}");
    }
    let LibraryReply::Tracks(page) = query::execute(
        &mut connection,
        LibraryCommand::Tracks {
            album_id: Some(3),
            page: page_request(Some(7), 1, 1, "artist:[Miles,Ornette]"),
        },
    )
    .unwrap() else {
        panic!("expected tracks");
    };
    assert_eq!((page.total, page.offset), (2, 1));
    assert_eq!(
        page.entries
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        [4]
    );
}

#[test]
fn structured_artist_album_projections_require_one_matching_track() {
    let mut connection = structured_filter_fixture();
    for (filter, artists, albums) in [
        ("artist:Miles & album:Collection", vec![3], vec![3]),
        ("artist:Miles & title:Peace", vec![], vec![]),
        ("artist:Ornette & title:Peace", vec![3], vec![3]),
        ("artist:Miles", vec![1, 3], vec![3, 1]),
        ("title:Hidden", vec![], vec![]),
    ] {
        let LibraryReply::Artists(page) = query::execute(
            &mut connection,
            LibraryCommand::Artists(page_request(Some(7), 0, 500, filter)),
        )
        .unwrap() else {
            panic!("expected artists");
        };
        assert_eq!(
            page.entries
                .iter()
                .map(|artist| artist.id)
                .collect::<Vec<_>>(),
            artists,
            "{filter}"
        );
        assert_eq!(page.total, artists.len() as u64);
        assert!(page.entries.iter().all(|artist| artist.album_count == 1));
        let LibraryReply::Albums(page) = query::execute(
            &mut connection,
            LibraryCommand::Albums {
                artist_id: None,
                page: page_request(Some(7), 0, 500, filter),
            },
        )
        .unwrap() else {
            panic!("expected albums");
        };
        assert_eq!(
            page.entries
                .iter()
                .map(|album| album.id)
                .collect::<Vec<_>>(),
            albums,
            "{filter}"
        );
        assert_eq!(page.total, albums.len() as u64);
        assert!(page.entries.iter().all(|album| album.track_count == 1));
    }
    let LibraryReply::Albums(page) = query::execute(
        &mut connection,
        LibraryCommand::Albums {
            artist_id: Some(1),
            page: page_request(Some(7), 0, 500, "artist:Miles album:Collection"),
        },
    )
    .unwrap() else {
        panic!("expected albums");
    };
    assert!(page.entries.is_empty());
}

#[test]
fn malformed_filters_are_typed_located_and_bounded_for_all_projections() {
    let mut connection = structured_filter_fixture();
    for filter in [
        "artist:",
        "genre:jazz",
        "artist:[]",
        "artist:[Miles,]",
        "artist:[,Miles]",
        "artist:[Miles Ornette]",
        "artist:[Miles",
        "artist:[\"\",Miles]",
        "title:\"\"",
        "title:\"   \"",
        "\"unterminated",
        "\"bad\\q\"",
        "(",
        "()",
        "title:Peace)",
        "title:Peace |",
        "& title:Peace",
        "title:Peace title:",
        "\"a\"\"b\"",
        "artist:(Miles | Ornette)",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([
        "x".repeat(1025),
        format!("{}x{}", "(".repeat(33), ")".repeat(33)),
        format!("{}x", "!".repeat(33)),
        vec!["x"; 129].join(" "),
    ]) {
        for command in [
            LibraryCommand::Artists(page_request(None, 0, 10, &filter)),
            LibraryCommand::Albums {
                artist_id: None,
                page: page_request(None, 0, 10, &filter),
            },
            LibraryCommand::Tracks {
                album_id: None,
                page: page_request(None, 0, 10, &filter),
            },
        ] {
            let error = classify(query::execute(&mut connection, command).unwrap_err());
            assert_eq!(error.kind, LibraryErrorKind::InvalidFilter, "{filter}");
            assert!(error.message.contains("UTF-8 byte "), "{}", error.message);
        }
    }
    let error = classify(
        query::execute(
            &mut connection,
            LibraryCommand::Tracks {
                album_id: None,
                page: page_request(None, 0, 10, "été & )"),
            },
        )
        .unwrap_err(),
    );
    assert!(error.message.contains("UTF-8 byte 8:"), "{}", error.message);
    // Roots are deliberately literal, not another structured filter surface.
    let LibraryReply::Roots(page) = query::execute(
        &mut connection,
        LibraryCommand::Roots(page_request(None, 0, 10, "artist:[]")),
    )
    .unwrap() else {
        panic!("expected roots");
    };
    assert!(page.entries.is_empty());
}

fn database(scratch: &Scratch) -> Connection {
    Connection::open(scratch.path("library.sqlite")).unwrap()
}

#[tokio::test]
async fn rescan_reextracts_assets_with_old_metadata_version() {
    let scratch = Scratch::new();
    flac(
        &scratch.path("music/a.flac"),
        &[("title", "A"), ("date", "1959-08-17"), ("genre", "Jazz")],
    );
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let connection = database(&scratch);
    let year: Option<i64> = connection
        .query_row("SELECT year FROM library_tracks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(year, Some(1959));
    connection
        .execute_batch(
            "UPDATE library_assets SET metadata_version=1; UPDATE library_tracks SET year=NULL;",
        )
        .unwrap();
    let scan = rescan(&library, root).await;
    assert_eq!((scan.updated, scan.unchanged), (1, 0));
    let (year, version): (Option<i64>, i64) = connection
        .query_row(
            "SELECT t.year,a.metadata_version FROM library_tracks t JOIN library_assets a ON a.id=t.asset_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((year, version), (Some(1959), 2));
    let scan = rescan(&library, root).await;
    assert_eq!((scan.updated, scan.unchanged), (0, 1));
    library.shutdown().unwrap();
}

#[tokio::test]
async fn date_added_uses_birth_time_and_survives_reindex() {
    let scratch = Scratch::new();
    let source = scratch.path("music/a.flac");
    flac(&source, &[("title", "A")]);
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let connection = database(&scratch);
    let added = |connection: &Connection| -> i64 {
        connection
            .query_row("SELECT date_added FROM library_tracks", [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let first = added(&connection);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(first > 0 && first <= now, "date_added {first}, now {now}");
    let created: Option<i64> = connection
        .query_row("SELECT created_ns FROM library_assets", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(created.is_some());
    let file = fs::OpenOptions::new().write(true).open(&source).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(file);
    let scan = rescan(&library, root).await;
    assert_eq!(scan.updated, 1);
    assert_eq!(added(&connection), first);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn genres_are_normalized_rows_in_tag_order() {
    let scratch = Scratch::new();
    flac(
        &scratch.path("music/a.flac"),
        &[("title", "A"), ("genre", "Soul;Jazz;soul")],
    );
    flac(
        &scratch.path("music/b.flac"),
        &[("title", "B"), ("genre", "jazz")],
    );
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let connection = database(&scratch);
    let genres = |title: &str| -> Vec<String> {
        let mut statement = connection
            .prepare(
                "SELECT g.name FROM library_track_genres tg
                JOIN library_genres g ON g.id=tg.genre_id
                JOIN library_tracks t ON t.id=tg.track_id
                WHERE t.title_sort=?1 ORDER BY tg.position",
            )
            .unwrap();
        statement
            .query_map([title], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(genres("a"), ["Soul", "Jazz"]);
    // One row per lowercased name; the first spelling stored wins.
    assert_eq!(genres("b"), ["Jazz"]);
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM library_genres", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    let search: String = connection
        .query_row(
            "SELECT search FROM library_tracks WHERE title_sort='a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(search.contains("soul\njazz"), "{search}");
    // Reindex with changed genres replaces the rows.
    flac(
        &scratch.path("music/a.flac"),
        &[("title", "A"), ("genre", "Blues")],
    );
    let scan = rescan(&library, root).await;
    assert_eq!(scan.updated, 1);
    assert_eq!(genres("a"), ["Blues"]);
    library.shutdown().unwrap();
}

async fn upgrade_cleared(scratch: &Scratch) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let connection = database(scratch);
    loop {
        let pending: i64 = connection
            .query_row("SELECT pending_upgrade FROM library_state", [], |row| {
                row.get(0)
            })
            .unwrap();
        if pending == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "upgrade rescan did not drain");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn scan_progress_does_not_change_revision_but_installs_do() {
    let scratch = Scratch::new();
    for name in ["a", "b", "c"] {
        flac(
            &scratch.path(&format!("music/{name}.flac")),
            &[("title", name)],
        );
    }
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let revision = library.state().revision;
    let scan = rescan(&library, root).await;
    assert_eq!((scan.unchanged, scan.updated, scan.processed), (3, 0, 3));
    assert_eq!(library.state().revision, revision);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(scratch.path("music/b.flac"))
        .unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(file);
    let scan = rescan(&library, root).await;
    assert_eq!(scan.updated, 1);
    assert!(library.state().revision > revision);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn stale_enqueue_revision_is_rejected_after_a_concurrent_install() {
    let scratch = Scratch::new();
    flac(&scratch.path("music/a.flac"), &[("title", "A")]);
    let library = scratch.library();
    let root = add(&library, &scratch.path("music")).await;
    finished(&library).await;
    let old = library.state().revision;
    let track = tracks(&library).await.entries.remove(0);
    flac(&scratch.path("music/b.flac"), &[("title", "B")]);
    rescan(&library, root).await;
    let connection = database(&scratch);
    let stale = resolve_track(&connection, track.id, old).unwrap_err();
    assert!(stale.downcast_ref::<StaleLibraryRevision>().is_some());
    assert!(resolve_track(&connection, track.id, library.state().revision).is_ok());
    library.shutdown().unwrap();
}

#[tokio::test]
async fn upgrade_flag_rescans_every_root_once_and_clears() {
    let scratch = Scratch::new();
    flac(&scratch.path("one/a.flac"), &[("title", "A")]);
    flac(&scratch.path("two/b.flac"), &[("title", "B")]);
    let library = scratch.library();
    add(&library, &scratch.path("one")).await;
    finished(&library).await;
    add(&library, &scratch.path("two")).await;
    finished(&library).await;
    library.shutdown().unwrap();
    database(&scratch)
        .execute_batch("UPDATE library_assets SET metadata_version=1; UPDATE library_state SET pending_upgrade=1;")
        .unwrap();
    let library = scratch.library();
    upgrade_cleared(&scratch).await;
    finished(&library).await;
    let connection = database(&scratch);
    let versions: Vec<i64> = connection
        .prepare("SELECT metadata_version FROM library_assets")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(versions, vec![db::METADATA_VERSION; 2]);
    let phases: Vec<ScanPhase> = connection
        .prepare("SELECT last_scan FROM library_roots")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|json| {
            serde_json::from_str::<ScanStatus>(&json.unwrap())
                .unwrap()
                .phase
        })
        .collect();
    assert_eq!(phases, vec![ScanPhase::Complete; 2]);
    library.shutdown().unwrap();
}

#[tokio::test]
async fn upgraded_tracks_without_date_added_are_not_recent() {
    // Task 6 adds the Recent source and finishes this with a source assertion.
    let scratch = Scratch::new();
    flac(&scratch.path("music/a.flac"), &[("title", "A")]);
    let library = scratch.library();
    add(&library, &scratch.path("music")).await;
    finished(&library).await;
    library.shutdown().unwrap();
    database(&scratch)
        .execute_batch(
            "UPDATE library_tracks SET date_added=0; UPDATE library_assets SET metadata_version=1;",
        )
        .unwrap();
    let library = scratch.library();
    let added: i64 = database(&scratch)
        .query_row("SELECT date_added FROM library_tracks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(added, 0);
    library.shutdown().unwrap();
}
