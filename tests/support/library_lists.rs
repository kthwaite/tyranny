//! v9 list queries through a real authenticated client and native indexing.
use super::{
    saved_playlists::{add, create, index, ok, queue, rescan},
    *,
};
use serde_json::{Value, json};
use tyranny::{
    listing::Grouping,
    play_order::RepeatMode,
    protocol::{Playlist, QueueSource, SourceKind},
};

fn list(source: Value) -> Value {
    json!({"source":source,"query":"","sort":[{"field":"title","descending":true}],"group":"none"})
}
fn list_tracks(list: Value, revision: Option<u64>, offset: usize, limit: usize) -> Value {
    json!({"command":"list_tracks","list":list,"revision":revision,"offset":offset,"limit":limit})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_tracks_round_trips_over_the_socket_and_rejects_stale_revision() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (_, tracks) = index(&scratch, &mut client, &["Alpha", "Beta", "Gamma"]).await;
    let revision = tracks.revision;
    let id = |title: &str| {
        tracks
            .entries
            .iter()
            .find(|track| track.metadata.title == title)
            .unwrap()
            .id
    };
    let library = list(json!({"kind":"library"}));
    let Response::ListTracks { page, totals } = ok(
        &mut client,
        list_tracks(library.clone(), Some(revision), 0, 2),
    )
    .await
    else {
        panic!("expected a list page");
    };
    assert_eq!((page.revision, page.total, page.offset), (revision, 3, 0));
    assert_eq!(
        page.entries
            .iter()
            .map(|row| (row.row_id, row.track.title.as_str()))
            .collect::<Vec<_>>(),
        [(id("Gamma"), "Gamma"), (id("Beta"), "Beta")]
    );
    let summary = &page.entries[0].track;
    assert_eq!(
        (
            summary.artist.as_str(),
            summary.album.as_str(),
            summary.format.as_str(),
            summary.duration_ms
        ),
        ("Fixture Artist", "Fixture Album", "WAV 16/48", Some(5000))
    );
    let totals = totals.expect("first page carries totals");
    assert_eq!((totals.tracks, totals.duration_ms), (3, 15_000));
    assert!(totals.size_bytes > 0);
    assert_eq!(
        (
            client.state.library.duration_ms,
            client.state.library.size_bytes
        ),
        (15_000, totals.size_bytes)
    );
    let Response::ListTracks { page, totals } = ok(
        &mut client,
        list_tracks(library.clone(), Some(revision), 2, 2),
    )
    .await
    else {
        panic!("expected a list page");
    };
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].row_id, id("Alpha"));
    assert_eq!(totals, None);
    client
        .reject(
            list_tracks(library.clone(), Some(revision - 1), 0, 2),
            ErrorCode::StaleLibraryRevision,
        )
        .await;
    client
        .reject(
            list_tracks(library.clone(), None, 0, 501),
            ErrorCode::InvalidCommand,
        )
        .await;

    // Playlist sources keep the playlist's own revision and error codes.
    let playlist = create(&mut client, "Mix").await;
    let playlist = add(
        &mut client,
        &playlist,
        revision,
        vec![id("Beta"), id("Alpha")],
    )
    .await;
    let source =
        |id: u64, revision: u64| list(json!({"kind":"playlist","id":id,"revision":revision}));
    let Response::ListTracks { page, .. } = ok(
        &mut client,
        list_tracks(source(playlist.id, playlist.revision), None, 0, 10),
    )
    .await
    else {
        panic!("expected a playlist list page");
    };
    assert_eq!(
        page.entries
            .iter()
            .map(|row| (row.track.id, row.available))
            .collect::<Vec<_>>(),
        [(id("Beta"), true), (id("Alpha"), true)]
    );
    client
        .reject(
            list_tracks(source(playlist.id, playlist.revision - 1), None, 0, 10),
            ErrorCode::StalePlaylistRevision,
        )
        .await;
    client
        .reject(
            list_tracks(source(999, 1), None, 0, 10),
            ErrorCode::InvalidCommand,
        )
        .await;

    // The other list commands route to the library worker.
    let Response::ListAlbums { page } = ok(
        &mut client,
        json!({"command":"list_albums","list":library,"sort":"year","descending":false,
            "revision":revision,"offset":0,"limit":10}),
    )
    .await
    else {
        panic!("expected albums");
    };
    assert_eq!(page.total, 1);
    let album = page.entries[0].clone();
    assert_eq!(
        (
            album.title.as_str(),
            album.track_count,
            album.format.as_str()
        ),
        ("Fixture Album", 3, "WAV 16/48")
    );
    let Response::AlbumSummaries {
        revision: summary_revision,
        entries,
    } = ok(
        &mut client,
        json!({"command":"album_summaries","album_ids":[album.id, 999],"revision":revision}),
    )
    .await
    else {
        panic!("expected album summaries");
    };
    assert_eq!((summary_revision, entries), (revision, vec![album.clone()]));
    let Response::ListArtists { page, compilations } = ok(
        &mut client,
        json!({"command":"list_artists","query":"","revision":null,"offset":0,"limit":10}),
    )
    .await
    else {
        panic!("expected artists");
    };
    assert_eq!(
        page.entries
            .iter()
            .map(|artist| (artist.name.as_str(), artist.album_count, artist.track_count))
            .collect::<Vec<_>>(),
        [("Fixture Artist", 1, 3)]
    );
    assert_eq!(compilations, None);
    let Response::ListGenres { page } = ok(
        &mut client,
        json!({"command":"list_genres","query":"","revision":null,"offset":0,"limit":10}),
    )
    .await
    else {
        panic!("expected genres");
    };
    assert_eq!(page.total, 0);
    let mut grouped = list(json!({"kind":"library"}));
    grouped["group"] = json!(Grouping::ArtistAlbum);
    let Response::ListGroups { total, groups, .. } = ok(
        &mut client,
        json!({"command":"list_groups","list":grouped,"revision":revision}),
    )
    .await
    else {
        panic!("expected group spans");
    };
    assert_eq!(total, 3);
    assert_eq!(
        groups
            .iter()
            .map(|group| (group.album_key, group.start, group.count))
            .collect::<Vec<_>>(),
        [(Some(album.id), 0, 3)]
    );
    client
        .reject(
            json!({"command":"list_groups","list":list(json!({"kind":"library"})),"revision":null}),
            ErrorCode::InvalidCommand,
        )
        .await;
    running.stop().await;
}

fn stamp(kind: SourceKind, name: &str) -> Option<QueueSource> {
    Some(QueueSource {
        kind,
        name: name.to_owned(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enqueue_playlist_stamps_source_and_one_run_id() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (_, tracks) = index(&scratch, &mut client, &["Alpha", "Beta", "Gamma", "Delta"]).await;
    let library = tracks.revision;
    let id = |title: &str| {
        tracks
            .entries
            .iter()
            .find(|track| track.metadata.title == title)
            .unwrap()
            .id
    };
    let mix = create(&mut client, "Mix").await;
    let mix = add(&mut client, &mix, library, vec![id("Beta"), id("Alpha")]).await;
    let expected = client.state.queue_revision;
    ok(
        &mut client,
        json!({"command":"enqueue_playlist","playlist_id":mix.id,"playlist_revision":mix.revision,
            "library_revision":library,"expected_revision":expected,"play":false}),
    )
    .await;
    let items = queue(&mut client).await;
    assert_eq!(items.len(), 2);
    let first = items[0].id;
    for (item, title) in items.iter().zip(["Beta", "Alpha"]) {
        assert_eq!(item.source, stamp(SourceKind::Playlist, "Mix"));
        assert_eq!((item.run_id, item.played), (Some(first), false));
        assert_eq!(
            (item.title.as_str(), item.artist.as_str()),
            (title, "Fixture Artist")
        );
        assert_eq!(item.album, "Fixture Album");
        assert_eq!(item.track_id, Some(id(title)));
        assert!(item.album_id.is_some());
        assert_eq!(item.duration_ms, Some(5000));
    }

    let catalog = client.state.playlists_revision;
    let smart = ok(
        &mut client,
        json!({"command":"create_smart_playlist","expected_revision":catalog,
            "name":"Live","query":"title:Gamma"}),
    )
    .await;
    let Response::PlaylistApplied {
        playlist: Some(smart),
        ..
    } = smart
    else {
        panic!("expected smart playlist");
    };
    let expected = client.state.queue_revision;
    ok(
        &mut client,
        json!({"command":"enqueue_playlist","playlist_id":smart.id,"playlist_revision":smart.revision,
            "library_revision":library,"expected_revision":expected,"play":false}),
    )
    .await;
    let items = queue(&mut client).await;
    assert_eq!(items.len(), 3);
    let smart_id = items[2].id;
    assert_eq!(items[2].source, stamp(SourceKind::Smart, "Live"));
    assert_eq!(items[2].run_id, Some(smart_id));
    assert_eq!(items[0].run_id, Some(first));

    let expected = client.state.queue_revision;
    ok(
        &mut client,
        json!({"command":"append_track","expected_revision":expected,
            "library_revision":library,"track_id":id("Delta")}),
    )
    .await;
    let path = scratch.path("direct.wav");
    tagged_wav(&path, 480, b"Direct");
    client.append(&path).await;
    let expected = client.state.queue_revision;
    ok(
        &mut client,
        json!({"command":"insert_tracks","expected_revision":expected,
            "library_revision":library,"track_ids":[id("Alpha"), id("Beta")],"before":null}),
    )
    .await;
    let items = queue(&mut client).await;
    assert_eq!(items.len(), 7);
    let summary = |index: usize| {
        (
            items[index].source.clone(),
            items[index].run_id == Some(items[index].id),
        )
    };
    assert_eq!(summary(3), (stamp(SourceKind::Drag, ""), true));
    assert_eq!(summary(4), (stamp(SourceKind::Direct, ""), true));
    let inserted = items[5].id;
    for index in [5, 6] {
        assert_eq!(items[index].source, stamp(SourceKind::Drag, ""));
        assert_eq!(items[index].run_id, Some(inserted));
    }
    assert_eq!(items[4].title, "direct");
    assert_eq!(items[4].track_id, None);
    running.stop().await;

    // Source, run and title survive a restart.
    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(queue(&mut restored).await, items);
    running.stop().await;
}

/// `tagged_wav` plus an ID3 chunk carrying an album ReplayGain tag.
fn album_gain_wav(path: &Path, title: &[u8], gain: &str) {
    tagged_wav(path, 240_000, title);
    let mut frame = Vec::new();
    let mut body = vec![0];
    body.extend_from_slice(b"REPLAYGAIN_ALBUM_GAIN\0");
    body.extend_from_slice(gain.as_bytes());
    frame.extend_from_slice(b"TXXX");
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&body);
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    let size = frame.len() as u32;
    tag.extend_from_slice(&[
        (size >> 21) as u8 & 0x7f,
        (size >> 14) as u8 & 0x7f,
        (size >> 7) as u8 & 0x7f,
        size as u8 & 0x7f,
    ]);
    tag.extend_from_slice(&frame);
    if tag.len() % 2 == 1 {
        tag.push(0);
    }
    let mut file = fs::read(path).unwrap();
    file.extend_from_slice(b"id3 ");
    file.extend_from_slice(&(tag.len() as u32).to_le_bytes());
    file.extend_from_slice(&tag);
    let riff = (file.len() - 8) as u32;
    file[4..8].copy_from_slice(&riff.to_le_bytes());
    fs::write(path, file).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn now_playing_reports_format_and_applied_gain() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let music = scratch.path("music");
    fs::create_dir(&music).unwrap();
    album_gain_wav(&music.join("Gained.wav"), b"Gained", "-6.00 dB");
    let root = ok(&mut client, json!({"command":"add_root","path":music})).await;
    assert!(matches!(root, Response::RootAdded { .. }));
    client
        .until(|state| state.library.scan.phase == tyranny::library_types::ScanPhase::Complete)
        .await;
    let Response::LibraryTracks { page } = ok(
        &mut client,
        json!({"command":"library_tracks","revision":null,"offset":0,"limit":10,"query":"","album_id":null}),
    )
    .await
    else {
        panic!("expected tracks");
    };
    assert_eq!(page.entries[0].metadata.album_gain_db, Some(-6.0));
    let track = &page.entries[0];
    let play = |client: &PlaybackClient| {
        json!({"command":"append_track","expected_revision":client.state.queue_revision,
            "library_revision":page.revision,"track_id":track.id})
    };

    // Gain mode off: the format is known, no gain was applied.
    let command = play(&client);
    ok(&mut client, command).await;
    ok(&mut client, json!({"command":"play","entry_id":1})).await;
    client.until(|state| state.now_playing.is_some()).await;
    let now = client.state.now_playing.clone().unwrap();
    assert_eq!(now.track_id, Some(track.id));
    assert_eq!(now.album_id, Some(track.album_id));
    assert_eq!(now.format, "WAV 16/48");
    assert_eq!(now.gain_db, None);

    ok(&mut client, json!({"command":"set_gain","mode":"album"})).await;
    client
        .until(|state| {
            state
                .now_playing
                .as_ref()
                .is_some_and(|now| now.gain_db.is_some())
        })
        .await;
    let now = client.state.now_playing.clone().unwrap();
    assert!((now.gain_db.unwrap() + 6.0).abs() < 1e-9);
    assert_eq!(
        (now.format.as_str(), now.track_id),
        ("WAV 16/48", Some(track.id))
    );

    // A path-appended entry has no indexed track.
    let loose = scratch.path("loose.wav");
    tagged_wav(&loose, 480, b"Loose");
    client.append(&loose).await;
    ok(&mut client, json!({"command":"play","entry_id":2})).await;
    client.until(|state| state.current_id == Some(2)).await;
    let now = client.state.now_playing.clone().unwrap();
    assert_eq!(
        (now.track_id, now.album_id, now.format.as_str()),
        (None, None, "")
    );
    running.stop().await;
}

fn songs() -> Value {
    json!({"source":{"kind":"library"},"query":"","sort":[{"field":"title","descending":false}],
        "group":"none"})
}
fn enqueue(
    client: &PlaybackClient,
    selection: Value,
    placement: &str,
    play: bool,
    name: &str,
) -> Value {
    json!({"command":"enqueue_tracks","selection":selection,
        "expected_revision":client.state.queue_revision,"placement":placement,"play":play,
        "source":{"kind":"songs","name":name}})
}
/// Every row of `list` as `(revision, track IDs)`, read in pages.
async fn list_track_ids(client: &mut PlaybackClient, list: Value) -> (u64, Vec<u64>) {
    let mut revision = None;
    let mut ids = Vec::new();
    loop {
        let Response::ListTracks { page, .. } =
            ok(client, list_tracks(list.clone(), revision, ids.len(), 500)).await
        else {
            panic!("expected a list page");
        };
        revision = Some(page.revision);
        ids.extend(page.entries.iter().map(|row| row.track.id));
        if ids.len() as u64 == page.total {
            return (page.revision, ids);
        }
    }
}
/// The whole queue, read in pages.
async fn whole_queue(client: &mut PlaybackClient) -> Vec<tyranny::protocol::QueueItem> {
    let mut items = Vec::new();
    loop {
        let Response::QueuePage { total, entries, .. } = ok(
            client,
            json!({"command":"queue_page","revision":null,"offset":items.len(),"limit":500}),
        )
        .await
        else {
            panic!("expected a queue page");
        };
        items.extend(entries);
        if items.len() == total {
            return items;
        }
    }
}
fn applied_entry(response: Response) -> u64 {
    match response {
        Response::Applied {
            entry_id: Some(id), ..
        } => id,
        response => panic!("expected an applied enqueue: {response:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_enqueue_of_600_rows_plays_from_clicked_row() {
    let scratch = Scratch::new();
    let music = scratch.path("music");
    fs::create_dir(&music).unwrap();
    for index in 0..600 {
        // The clicked row plays long enough to stay current through the restart.
        let frames = if index == 120 { 240_000 } else { 480 };
        tagged_wav(
            &music.join(format!("{index:04}.wav")),
            frames,
            format!("Song {index:04}").as_bytes(),
        );
    }
    // Long entries outside the library: history, current and upcoming.
    let outside: Vec<_> = ["History", "Current", "Upcoming"]
        .iter()
        .map(|name| {
            let path = scratch.path(&format!("{name}.wav"));
            tagged_wav(&path, 240_000, name.as_bytes());
            path
        })
        .collect();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    ok(&mut client, json!({"command":"add_root","path":music})).await;
    client
        .until(|state| state.library.scan.phase == tyranny::library_types::ScanPhase::Complete)
        .await;
    for path in &outside {
        client.append(path).await;
    }
    ok(&mut client, json!({"command":"play","entry_id":2})).await;
    let (revision, rows) = list_track_ids(&mut client, songs()).await;
    assert_eq!(rows.len(), 600);

    let selection = |first_row_id: u64, library_revision: u64| {
        json!({"kind":"list","list":songs(),"library_revision":library_revision,
            "ranges":[[120, u64::MAX]],"excluded":[],"first_row_id":first_row_id})
    };
    // Stale or malformed requests change nothing.
    let before = whole_queue(&mut client).await;
    let command = enqueue(
        &client,
        selection(rows[120], revision - 1),
        "replace_upcoming",
        true,
        "Songs",
    );
    client
        .reject(command, ErrorCode::StaleLibraryRevision)
        .await;
    let command = enqueue(
        &client,
        selection(rows[121], revision),
        "replace_upcoming",
        true,
        "Songs",
    );
    client
        .reject(command, ErrorCode::StaleLibraryRevision)
        .await;
    let command = enqueue(
        &client,
        selection(rows[120], revision),
        "replace_upcoming",
        true,
        &"n".repeat(257),
    );
    client.reject(command, ErrorCode::InvalidCommand).await;
    let mut command = enqueue(
        &client,
        selection(rows[120], revision),
        "replace_upcoming",
        true,
        "Songs",
    );
    command["expected_revision"] = json!(client.state.queue_revision - 1);
    client.reject(command, ErrorCode::StaleRevision).await;
    assert_eq!(whole_queue(&mut client).await, before);
    assert_eq!(client.state.current_id, Some(2));

    let expected = client.state.queue_revision;
    let command = enqueue(
        &client,
        selection(rows[120], revision),
        "replace_upcoming",
        true,
        "Songs",
    );
    let first = applied_entry(ok(&mut client, command).await);
    assert_eq!(client.state.current_id, Some(first));
    assert_eq!(client.state.queue_revision, expected + 1);
    assert_eq!(client.state.queue_length, 482);
    let items = whole_queue(&mut client).await;
    // History and the old current entry stay; the old upcoming entry is gone.
    assert_eq!(
        items[..2]
            .iter()
            .map(|item| (item.id, item.title.as_str()))
            .collect::<Vec<_>>(),
        [(1, "History"), (2, "Current")]
    );
    let run = &items[2..];
    assert_eq!(
        run.iter().map(|item| item.track_id).collect::<Vec<_>>(),
        rows[120..].iter().map(|id| Some(*id)).collect::<Vec<_>>()
    );
    for (index, item) in run.iter().enumerate() {
        assert_eq!(item.id, first + index as u64);
        assert_eq!(item.source, stamp(SourceKind::Songs, "Songs"));
        // The first entry is current, so it is played.
        assert_eq!((item.run_id, item.played), (Some(first), index == 0));
    }
    assert_eq!(run[0].title, "Song 0120");
    running.stop().await;

    // The run survives a restart in order.
    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.current_id, Some(first));
    assert_eq!(whole_queue(&mut restored).await, items);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_mode_inserts_run_before_existing_upcoming() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (_, tracks) = index(&scratch, &mut client, &["Alpha", "Beta", "Gamma"]).await;
    let id = |title: &str| {
        tracks
            .entries
            .iter()
            .find(|track| track.metadata.title == title)
            .unwrap()
            .id
    };
    for name in ["Current", "Later 1", "Later 2"] {
        let path = scratch.path(&format!("{name}.wav"));
        tagged_wav(&path, 240_000, name.as_bytes());
        client.append(&path).await;
    }
    ok(&mut client, json!({"command":"play","entry_id":1})).await;
    let (revision, rows) = list_track_ids(&mut client, songs()).await;
    assert_eq!(rows, [id("Alpha"), id("Beta"), id("Gamma")]);

    // Keep: the run plays now and the old upcoming entries follow it.
    let selection = json!({"kind":"list","list":songs(),"library_revision":revision,
        "ranges":[[1, u64::MAX]],"excluded":[],"first_row_id":id("Beta")});
    let command = enqueue(&client, selection, "next", true, "Songs");
    let first = applied_entry(ok(&mut client, command).await);
    assert_eq!(first, 4);
    assert_eq!(client.state.current_id, Some(4));
    let order = |items: &[tyranny::protocol::QueueItem]| {
        items
            .iter()
            .map(|item| (item.id, item.title.clone()))
            .collect::<Vec<_>>()
    };
    let named = |entries: &[(u64, &str)]| {
        entries
            .iter()
            .map(|(id, title)| (*id, title.to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        order(&whole_queue(&mut client).await),
        named(&[
            (1, "Current"),
            (4, "Beta"),
            (5, "Gamma"),
            (2, "Later 1"),
            (3, "Later 2")
        ])
    );

    // Next without play goes straight after the current entry; Append goes last.
    let alpha = json!({"kind":"ids","library_revision":revision,"track_ids":[id("Alpha")]});
    let command = enqueue(&client, alpha.clone(), "next", false, "");
    assert_eq!(applied_entry(ok(&mut client, command).await), 6);
    let command = enqueue(&client, alpha, "append", false, "");
    assert_eq!(applied_entry(ok(&mut client, command).await), 7);
    assert_eq!(client.state.current_id, Some(4));
    let items = whole_queue(&mut client).await;
    assert_eq!(
        order(&items),
        named(&[
            (1, "Current"),
            (4, "Beta"),
            (6, "Alpha"),
            (5, "Gamma"),
            (2, "Later 1"),
            (3, "Later 2"),
            (7, "Alpha")
        ])
    );
    assert_eq!(items[2].run_id, Some(6));
    assert_eq!(items[3].run_id, Some(4));
    running.stop().await;
}

fn add_selection(playlist: &Playlist, selection: Value, before: Option<u64>) -> Value {
    json!({"command":"playlist_add_selection","playlist_id":playlist.id,
        "expected_revision":playlist.revision,"selection":selection,"before":before})
}
/// Every entry of a playlist as `(entry ID, track ID)`, read in pages.
async fn playlist_rows(client: &mut PlaybackClient, id: u64) -> Vec<(u64, u64)> {
    let mut rows = Vec::new();
    let mut revisions = (None, None);
    loop {
        let Response::PlaylistEntries {
            library_revision,
            page,
            ..
        } = ok(
            client,
            json!({"command":"playlist_entries","playlist_id":id,"revision":revisions.0,
                "library_revision":revisions.1,"offset":rows.len(),"limit":500,"query":""}),
        )
        .await
        else {
            panic!("expected playlist entries");
        };
        revisions = (Some(page.revision), Some(library_revision));
        rows.extend(page.entries.iter().map(|entry| (entry.id, entry.track_id)));
        if rows.len() as u64 == page.total {
            return rows;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlist_add_selection_adds_all_matching_rows_atomically() {
    let scratch = Scratch::new();
    let music = scratch.path("music");
    fs::create_dir(&music).unwrap();
    for index in 0..700 {
        tagged_wav(
            &music.join(format!("{index:04}.wav")),
            480,
            format!("Song {index:04}").as_bytes(),
        );
    }
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    ok(&mut client, json!({"command":"add_root","path":music})).await;
    client
        .until(|state| state.library.scan.phase == tyranny::library_types::ScanPhase::Complete)
        .await;
    let (revision, rows) = list_track_ids(&mut client, songs()).await;
    assert_eq!(rows.len(), 700);
    let playlist = create(&mut client, "Everything").await;
    let selection = |first_row_id: u64, library_revision: u64| {
        json!({"kind":"list","list":songs(),"library_revision":library_revision,
            "ranges":[[0, u64::MAX]],"excluded":[rows[5]],"first_row_id":first_row_id})
    };

    // Stale or malformed requests change nothing.
    client
        .reject(
            add_selection(&playlist, selection(rows[0], revision - 1), None),
            ErrorCode::StaleLibraryRevision,
        )
        .await;
    client
        .reject(
            add_selection(&playlist, selection(rows[1], revision), None),
            ErrorCode::StaleLibraryRevision,
        )
        .await;
    let mut stale = playlist.clone();
    stale.revision -= 1;
    client
        .reject(
            add_selection(&stale, selection(rows[0], revision), None),
            ErrorCode::StalePlaylistRevision,
        )
        .await;
    client
        .reject(
            add_selection(&playlist, selection(rows[0], revision), Some(999)),
            ErrorCode::InvalidCommand,
        )
        .await;
    assert!(playlist_rows(&mut client, playlist.id).await.is_empty());

    let Response::PlaylistApplied {
        playlist: Some(added),
        ..
    } = ok(
        &mut client,
        add_selection(&playlist, selection(rows[0], revision), None),
    )
    .await
    else {
        panic!("expected playlist change");
    };
    assert_eq!(added.revision, playlist.revision + 1);
    assert_eq!(added.entry_count, Some(699));
    let entries = playlist_rows(&mut client, playlist.id).await;
    let mut expected = rows.clone();
    expected.remove(5);
    assert_eq!(
        entries.iter().map(|(_, track)| *track).collect::<Vec<_>>(),
        expected
    );

    // A second selection lands before an existing occurrence.
    let tail = json!({"kind":"list","list":songs(),"library_revision":revision,
        "ranges":[[698, u64::MAX]],"excluded":[],"first_row_id":rows[698]});
    let before = entries[1].0;
    ok(&mut client, add_selection(&added, tail, Some(before))).await;
    let entries = playlist_rows(&mut client, playlist.id).await;
    assert_eq!(
        entries[..4]
            .iter()
            .map(|(_, track)| *track)
            .collect::<Vec<_>>(),
        [rows[0], rows[698], rows[699], rows[1]]
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlist_add_selection_rejects_smart_destinations() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    index(&scratch, &mut client, &["Alpha", "Beta"]).await;
    let (revision, rows) = list_track_ids(&mut client, songs()).await;
    let catalog = client.state.playlists_revision;
    let Response::PlaylistApplied {
        playlist: Some(smart),
        ..
    } = ok(
        &mut client,
        json!({"command":"create_smart_playlist","expected_revision":catalog,
            "name":"Live","query":"title:Alpha"}),
    )
    .await
    else {
        panic!("expected smart playlist");
    };
    let selection = json!({"kind":"list","list":songs(),"library_revision":revision,
        "ranges":[[0, u64::MAX]],"excluded":[],"first_row_id":rows[0]});
    client
        .reject(
            add_selection(&smart, selection, None),
            ErrorCode::InvalidCommand,
        )
        .await;
    running.stop().await;
}

/// Appends a long tagged WAV from outside the library for each name.
async fn append_long(scratch: &Scratch, client: &mut PlaybackClient, names: &[&str]) {
    for name in names {
        let path = scratch.path(&format!("{name}.wav"));
        tagged_wav(&path, 240_000, name.as_bytes());
        client.append(&path).await;
    }
}
fn ids_and_played(items: &[tyranny::protocol::QueueItem]) -> Vec<(u64, bool)> {
    items.iter().map(|item| (item.id, item.played)).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shuffle_upcoming_permutes_only_entries_after_current() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let upcoming: Vec<String> = (0..20)
        .map(|index| format!("Upcoming {index:02}"))
        .collect();
    let mut names = vec!["History", "Current"];
    names.extend(upcoming.iter().map(String::as_str));
    append_long(&scratch, &mut client, &names).await;
    // Play, Previous and Next each make an entry current, and so played.
    ok(&mut client, json!({"command":"play","entry_id":2})).await;
    ok(&mut client, json!({"command":"previous"})).await;
    assert_eq!(client.state.current_id, Some(1));
    ok(&mut client, json!({"command":"next"})).await;
    assert_eq!(client.state.current_id, Some(2));
    let before = whole_queue(&mut client).await;
    assert_eq!(
        ids_and_played(&before[..3]),
        [(1, true), (2, true), (3, false)]
    );

    let expected = client.state.queue_revision;
    client
        .reject(
            json!({"command":"shuffle_upcoming","expected_revision":expected - 1}),
            ErrorCode::StaleRevision,
        )
        .await;
    assert_eq!(whole_queue(&mut client).await, before);
    ok(
        &mut client,
        json!({"command":"shuffle_upcoming","expected_revision":expected}),
    )
    .await;
    assert_eq!(client.state.queue_revision, expected + 1);
    assert_eq!(client.state.current_id, Some(2));
    let after = whole_queue(&mut client).await;
    assert_eq!(after[..2], before[..2]);
    let order: Vec<u64> = after[2..].iter().map(|item| item.id).collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (3..=22).collect::<Vec<_>>());
    // 20! orders: the identity means the command did not shuffle.
    assert_ne!(order, sorted);
    assert!(after[2..].iter().all(|item| !item.played));
    running.stop().await;

    // The shuffled order is durable.
    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(whole_queue(&mut restored).await, after);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_upcoming_creates_regular_playlist() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (root, tracks) = index(&scratch, &mut client, &["Alpha", "Beta", "Gamma"]).await;
    let library = tracks.revision;
    let track = |title: &str| {
        tracks
            .entries
            .iter()
            .find(|track| track.metadata.title == title)
            .unwrap()
    };
    let id = |title: &str| track(title).id;
    let append_track = |client: &PlaybackClient, track: u64| {
        json!({"command":"append_track","expected_revision":client.state.queue_revision,
            "library_revision":library,"track_id":track})
    };
    // Current (track Gamma), then Alpha, a loose file without a track, and Beta.
    let command = append_track(&client, id("Gamma"));
    ok(&mut client, command).await;
    let command = append_track(&client, id("Alpha"));
    ok(&mut client, command).await;
    append_long(&scratch, &mut client, &["Loose"]).await;
    let command = append_track(&client, id("Beta"));
    ok(&mut client, command).await;
    ok(&mut client, json!({"command":"play","entry_id":1})).await;
    let save = |client: &PlaybackClient, name: &str| {
        json!({"command":"save_upcoming","expected_revision":client.state.queue_revision,
            "expected_playlists_revision":client.state.playlists_revision,"name":name})
    };

    // Stale revisions change nothing.
    let catalog = client.state.playlists_revision;
    let mut stale = save(&client, "Up Next");
    stale["expected_playlists_revision"] = json!(catalog - 1);
    client.reject(stale, ErrorCode::StalePlaylistRevision).await;
    let mut stale = save(&client, "Up Next");
    stale["expected_revision"] = json!(client.state.queue_revision - 1);
    client.reject(stale, ErrorCode::StaleRevision).await;
    assert_eq!(client.state.playlists_revision, catalog);

    let command = save(&client, "Up Next");
    let Response::PlaylistApplied {
        playlist: Some(saved),
        ..
    } = ok(&mut client, command).await
    else {
        panic!("expected a saved playlist");
    };
    assert_eq!(client.state.playlists_revision, catalog + 1);
    assert_eq!(saved.name, "Up Next");
    assert_eq!(saved.kind, tyranny::protocol::PlaylistKind::Regular);
    assert_eq!(saved.entry_count, Some(2));
    assert_eq!(
        playlist_rows(&mut client, saved.id)
            .await
            .iter()
            .map(|(_, track)| *track)
            .collect::<Vec<_>>(),
        [id("Alpha"), id("Beta")]
    );
    // Playlist names stay unique, ignoring case.
    let command = save(&client, "up next");
    client.reject(command, ErrorCode::InvalidCommand).await;

    // An unavailable track is skipped and does not block the save.
    fs::remove_file(&track("Beta").path).unwrap();
    rescan(&mut client, root).await;
    let command = save(&client, "Partial");
    let Response::PlaylistApplied {
        playlist: Some(partial),
        ..
    } = ok(&mut client, command).await
    else {
        panic!("expected a saved playlist");
    };
    assert_eq!(partial.entry_count, Some(1));
    assert_eq!(
        playlist_rows(&mut client, partial.id)
            .await
            .iter()
            .map(|(_, track)| *track)
            .collect::<Vec<_>>(),
        [id("Alpha")]
    );
    // With no available track left there is nothing to save.
    fs::remove_file(&track("Alpha").path).unwrap();
    rescan(&mut client, root).await;
    let catalog = client.state.playlists_revision;
    let command = save(&client, "Nothing");
    client.reject(command, ErrorCode::InvalidCommand).await;
    assert_eq!(client.state.playlists_revision, catalog);

    // Clear Upcoming keeps the current entry playing.
    let expected = client.state.queue_revision;
    client
        .reject(
            json!({"command":"clear_upcoming","expected_revision":expected - 1}),
            ErrorCode::StaleRevision,
        )
        .await;
    ok(
        &mut client,
        json!({"command":"clear_upcoming","expected_revision":expected}),
    )
    .await;
    assert_eq!(client.state.queue_revision, expected + 1);
    assert_eq!(
        (client.state.current_id, client.state.queue_length),
        (Some(1), 1)
    );
    assert!(matches!(
        client.state.transport,
        Transport::Playing | Transport::Buffering
    ));
    assert_eq!(ids_and_played(&whole_queue(&mut client).await), [(1, true)]);
    // With nothing upcoming there is nothing to save.
    let command = save(&client, "Empty");
    client.reject(command, ErrorCode::InvalidCommand).await;
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn played_prefix_is_trimmed_to_100() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    // 105 entries of 0.05 s, then a long current entry and one unplayed entry.
    for index in 0..105 {
        let path = scratch.path(&format!("{index:03}.wav"));
        tagged_wav(&path, 2_400, format!("Short {index:03}").as_bytes());
        client.append(&path).await;
    }
    append_long(&scratch, &mut client, &["Current", "Upcoming"]).await;
    assert_eq!(client.state.queue_length, 107);
    ok(&mut client, json!({"command":"play","entry_id":1})).await;
    client
        .until(|state| state.current_id.is_some_and(|id| id >= 50))
        .await;
    let revision = client.state.queue_revision;
    client
        .until(|state| state.current_id == Some(106) && state.queue_length == 102)
        .await;
    // Each trim bumps the queue revision like a removal.
    assert!(client.state.queue_revision > revision);
    let items = whole_queue(&mut client).await;
    let mut expected: Vec<(u64, bool)> = (6..=106).map(|id| (id, true)).collect();
    expected.push((107, false));
    assert_eq!(ids_and_played(&items), expected);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn play_on_an_early_entry_trims_played_entries_after_it() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    // An unplayed entry of 0.5 s, then 101 entries of 0.025 s, then a long one.
    let early = scratch.path("early.wav");
    tagged_wav(&early, 24_000, b"Early");
    client.append(&early).await;
    for index in 0..101 {
        let path = scratch.path(&format!("{index:03}.wav"));
        tagged_wav(&path, 1_200, format!("Short {index:03}").as_bytes());
        client.append(&path).await;
    }
    append_long(&scratch, &mut client, &["Tail"]).await;
    ok(&mut client, json!({"command":"play","entry_id":2})).await;
    client
        .until(|state| state.current_id.is_some_and(|id| id >= 50))
        .await;
    // Entry 2 is trimmed when the tail (103) becomes current.
    client
        .until(|state| state.current_id == Some(103) && state.queue_length == 102)
        .await;

    // Playing the early entry trims entry 3, which is after it in the queue.
    let revision = client.state.queue_revision;
    ok(&mut client, json!({"command":"play","entry_id":1})).await;
    assert_eq!(client.state.current_id, Some(1));
    assert_eq!(client.state.queue_revision, revision + 1);
    let items = whole_queue(&mut client).await;
    let mut expected = vec![(1, true)];
    expected.extend((4..=103).map(|id| (id, true)));
    assert_eq!(ids_and_played(&items), expected);
    // Playback continues through the kept entries after the early one to the
    // tail. Each is played again; none is trimmed, as 100 others stay played.
    client.until(|state| state.current_id == Some(103)).await;
    assert_eq!(client.state.queue_length, 101);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeat_and_shuffle_settings_survive_restart() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    assert_eq!(client.state.settings.repeat, RepeatMode::Off);
    assert!(!client.state.settings.shuffle);
    ok(&mut client, json!({"command":"set_repeat","mode":"one"})).await;
    assert_eq!(client.state.settings.repeat, RepeatMode::One);
    ok(&mut client, json!({"command":"set_repeat","mode":"all"})).await;
    ok(&mut client, json!({"command":"set_shuffle","enabled":true})).await;
    assert_eq!(client.state.settings.repeat, RepeatMode::All);
    assert!(client.state.settings.shuffle);
    running.stop().await;

    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.settings.repeat, RepeatMode::All);
    assert!(restored.state.settings.shuffle);
    ok(
        &mut restored,
        json!({"command":"set_shuffle","enabled":false}),
    )
    .await;
    ok(&mut restored, json!({"command":"set_repeat","mode":"off"})).await;
    running.stop().await;

    let running = Running::start(&scratch).await;
    let restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.settings.repeat, RepeatMode::Off);
    assert!(!restored.state.settings.shuffle);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn volume_setting_persists_and_rejects_over_100() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    assert_eq!(client.state.settings.volume, 100);
    ok(&mut client, json!({"command":"set_volume","percent":35})).await;
    assert_eq!(client.state.settings.volume, 35);
    client
        .reject(
            json!({"command":"set_volume","percent":101}),
            ErrorCode::InvalidCommand,
        )
        .await;
    assert_eq!(client.state.settings.volume, 35);
    running.stop().await;

    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.settings.volume, 35);
    ok(&mut restored, json!({"command":"set_volume","percent":0})).await;
    ok(&mut restored, json!({"command":"set_volume","percent":100})).await;
    running.stop().await;

    let running = Running::start(&scratch).await;
    let restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.settings.volume, 100);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeat_all_wrap_starts_a_fresh_pass_over_the_run() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let music = scratch.path("music");
    fs::create_dir(&music).unwrap();
    // One-second tracks: each current entry is published at least once.
    for name in ["One", "Two"] {
        tagged_wav(&music.join(format!("{name}.wav")), 48_000, name.as_bytes());
    }
    ok(&mut client, json!({"command":"add_root","path":music})).await;
    client
        .until(|state| state.library.scan.phase == tyranny::library_types::ScanPhase::Complete)
        .await;
    let (library, rows) = list_track_ids(&mut client, songs()).await;
    ok(&mut client, json!({"command":"set_repeat","mode":"all"})).await;
    let command = json!({"command":"enqueue_tracks",
        "selection":{"kind":"ids","library_revision":library,"track_ids":rows},
        "expected_revision":client.state.queue_revision,"placement":"replace_upcoming",
        "play":true,"source":{"kind":"album","name":"Fixture Album"}});
    let first = applied_entry(ok(&mut client, command).await);
    let second = first + 1;
    append_long(&scratch, &mut client, &["Following"]).await;
    client.until(|state| state.current_id == Some(second)).await;
    client.until(|state| state.current_id == Some(first)).await;
    // The wrap cleared the run's played state; only the new current entry is played.
    let items = whole_queue(&mut client).await;
    assert_eq!(
        ids_and_played(&items),
        [(first, true), (second, false), (second + 1, false)]
    );
    running.stop().await;
}

/// Indexes long tracks named `names`, and returns the library revision and
/// their track IDs in title order.
async fn index_songs(
    scratch: &Scratch,
    client: &mut PlaybackClient,
    names: &[&str],
) -> (u64, Vec<u64>) {
    index(scratch, client, names).await;
    list_track_ids(client, songs()).await
}
fn play_ids(client: &PlaybackClient, library: u64, tracks: &[u64]) -> Value {
    json!({"command":"enqueue_tracks",
        "selection":{"kind":"ids","library_revision":library,"track_ids":tracks},
        "expected_revision":client.state.queue_revision,"placement":"replace_upcoming",
        "play":true,"source":{"kind":"album","name":"Fixture Album"}})
}
/// Presses Next and returns the new current entry.
async fn next(client: &mut PlaybackClient) -> u64 {
    ok(client, json!({"command":"next"})).await;
    client.state.current_id.unwrap()
}
/// `(id, played)` for the queue entries `run` names.
async fn run_played(client: &mut PlaybackClient, run: &[u64]) -> Vec<(u64, bool)> {
    let items = whole_queue(client).await;
    run.iter()
        .map(|id| {
            (
                *id,
                items.iter().find(|item| item.id == *id).unwrap().played,
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_at_the_end_of_a_repeat_all_run_starts_a_fresh_pass() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (library, tracks) = index_songs(&scratch, &mut client, &["Alpha", "Beta", "Gamma"]).await;
    ok(&mut client, json!({"command":"set_repeat","mode":"all"})).await;

    // In order: Next from the run's last entry wraps and empties its history.
    let command = play_ids(&client, library, &tracks);
    let first = applied_entry(ok(&mut client, command).await);
    let run = [first, first + 1, first + 2];
    assert_eq!(next(&mut client).await, run[1]);
    assert_eq!(next(&mut client).await, run[2]);
    assert_eq!(next(&mut client).await, run[0]);
    assert_eq!(
        run_played(&mut client, &run).await,
        [(run[0], true), (run[1], false), (run[2], false)]
    );
    assert_eq!(next(&mut client).await, run[1]);

    // Shuffled: the second pass plays each entry once, from emptied history.
    ok(&mut client, json!({"command":"set_shuffle","enabled":true})).await;
    let command = play_ids(&client, library, &tracks);
    let first = applied_entry(ok(&mut client, command).await);
    let run = [first, first + 1, first + 2];
    let mut heard = vec![first, next(&mut client).await, next(&mut client).await];
    heard.sort_unstable();
    assert_eq!(heard, run);
    let wrapped = next(&mut client).await;
    let played: Vec<(u64, bool)> = run.iter().map(|id| (*id, *id == wrapped)).collect();
    assert_eq!(run_played(&mut client, &run).await, played);
    let mut heard = vec![wrapped, next(&mut client).await, next(&mut client).await];
    heard.sort_unstable();
    assert_eq!(heard, run);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_and_previous_under_repeat_one_move_to_other_entries() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    append_long(&scratch, &mut client, &["One", "Two", "Three"]).await;
    ok(&mut client, json!({"command":"play","entry_id":1})).await;
    ok(&mut client, json!({"command":"set_repeat","mode":"one"})).await;
    assert_eq!(next(&mut client).await, 2);
    assert_eq!(next(&mut client).await, 3);
    ok(&mut client, json!({"command":"previous"})).await;
    assert_eq!(client.state.current_id, Some(2));
    assert_eq!(next(&mut client).await, 3);
    // As Repeat All would, Next wraps within the context.
    assert_eq!(next(&mut client).await, 1);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shuffled_play_of_a_list_without_a_clicked_row_starts_at_a_pick() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let names: Vec<String> = (0..10).map(|index| format!("Song {index:02}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let (library, tracks) = index_songs(&scratch, &mut client, &names).await;
    ok(&mut client, json!({"command":"set_shuffle","enabled":true})).await;
    let list = |first_row_id: Option<u64>| {
        json!({"kind":"list","list":songs(),"library_revision":library,
            "ranges":[[0, u64::MAX]],"excluded":[],"first_row_id":first_row_id})
    };
    // A clicked row, or IDs, start at the first new entry.
    for selection in [
        list(Some(tracks[0])),
        json!({"kind":"ids","library_revision":library,"track_ids":tracks}),
    ] {
        let command = enqueue(&client, selection, "replace_upcoming", true, "Songs");
        let first = applied_entry(ok(&mut client, command).await);
        assert_eq!(client.state.current_id, Some(first));
    }
    // Without one, each play starts at a shuffle pick within the new run.
    // Ten plays all starting at the first entry has odds of 10^-10.
    let mut starts = Vec::new();
    for _ in 0..10 {
        let command = enqueue(&client, list(None), "replace_upcoming", true, "Songs");
        let first = applied_entry(ok(&mut client, command).await);
        let current = client.state.current_id.unwrap();
        assert!((first..first + 10).contains(&current));
        starts.push(current - first);
    }
    assert!(starts.iter().any(|offset| *offset != 0), "{starts:?}");
    running.stop().await;
}
