//! Saved playlists through real authenticated clients, native indexing and the
//! clocked capture owner. No direct command dispatch or private engine wiring.
use super::*;
use serde_json::{Value, json};
use tyranny::{
    library_types::{Page, ScanPhase, Track},
    protocol::{Playlist, PlaylistEntry, QueueItem},
};

pub(super) async fn ok(client: &mut PlaybackClient, command: Value) -> Response {
    match client.request(command).await {
        Reply::Ok { result } => {
            match &result {
                Response::Snapshot { snapshot }
                | Response::Applied { snapshot, .. }
                | Response::PlaylistApplied { snapshot, .. } => {
                    assert!(snapshot.sequence >= client.state.sequence);
                    client.state = snapshot.as_ref().clone();
                }
                _ => {}
            }
            result
        }
        reply => panic!("playlist request failed: {reply:?}"),
    }
}
async fn change(client: &mut PlaybackClient, command: Value) -> Option<Playlist> {
    match ok(client, command).await {
        Response::PlaylistApplied { playlist, .. } => playlist,
        response => panic!("expected playlist change: {response:?}"),
    }
}
pub(super) async fn create(client: &mut PlaybackClient, name: &str) -> Playlist {
    let revision = client.state.playlists_revision;
    change(
        client,
        json!({"command":"create_playlist","expected_revision":revision,"name":name}),
    )
    .await
    .unwrap()
}
pub(super) async fn add(
    client: &mut PlaybackClient,
    playlist: &Playlist,
    library: u64,
    ids: Vec<u64>,
) -> Playlist {
    change(client, json!({"command":"playlist_add","playlist_id":playlist.id,"expected_revision":playlist.revision,"library_revision":library,"track_ids":ids,"before":null})).await.unwrap()
}
async fn entries(client: &mut PlaybackClient, id: u64) -> (u64, Page<PlaylistEntry>) {
    match ok(client, json!({"command":"playlist_entries","playlist_id":id,"revision":null,"library_revision":null,"offset":0,"limit":500,"query":""})).await {
        Response::PlaylistEntries { playlist_id, library_revision, page } => {
            assert_eq!(playlist_id, id);
            (library_revision, page)
        }
        response => panic!("expected playlist entries: {response:?}"),
    }
}
async fn catalog(client: &mut PlaybackClient, query: &str) -> Page<Playlist> {
    match ok(
        client,
        json!({"command":"saved_playlists","revision":null,"offset":0,"limit":100,"query":query}),
    )
    .await
    {
        Response::SavedPlaylists { page } => page,
        response => panic!("expected saved playlists: {response:?}"),
    }
}
async fn queue(client: &mut PlaybackClient) -> Vec<QueueItem> {
    match ok(
        client,
        json!({"command":"queue_page","revision":null,"offset":0,"limit":500}),
    )
    .await
    {
        Response::QueuePage { entries, .. } => entries,
        response => panic!("expected queue: {response:?}"),
    }
}
pub(super) async fn index(
    scratch: &Scratch,
    client: &mut PlaybackClient,
    names: &[&str],
) -> (u64, Page<Track>) {
    let music = scratch.path("music");
    fs::create_dir(&music).unwrap();
    for name in names {
        tagged_wav(&music.join(format!("{name}.wav")), 240_000, name.as_bytes());
    }
    let root = match ok(client, json!({"command":"add_root","path":music})).await {
        Response::RootAdded { root } => root,
        response => panic!("expected indexed root: {response:?}"),
    };
    client
        .until(|state| state.library.scan.phase == ScanPhase::Complete)
        .await;
    match ok(client, json!({"command":"library_tracks","revision":null,"offset":0,"limit":500,"query":"","album_id":null})).await {
        Response::LibraryTracks { page } => {
            assert_eq!(page.total as usize, names.len());
            (root.id, page)
        }
        response => panic!("expected indexed tracks: {response:?}"),
    }
}
async fn rescan(client: &mut PlaybackClient, root: u64) {
    let previous = client.state.library.revision;
    ok(client, json!({"command":"rescan","root_id":root})).await;
    client
        .until(|state| {
            state.library.revision > previous && state.library.scan.phase == ScanPhase::Complete
        })
        .await;
}
fn load(playlist: &Playlist, library: u64, queue_revision: u64, play: bool) -> Value {
    json!({"command":"enqueue_playlist","playlist_id":playlist.id,"playlist_revision":playlist.revision,
        "library_revision":library,"expected_revision":queue_revision,"play":play})
}
fn unchanged_playback(before: &ControlState, after: &ControlState) {
    assert_eq!(after.queue_revision, before.queue_revision);
    assert_eq!(after.queue_length, before.queue_length);
    assert_eq!(after.current_id, before.current_id);
    assert_eq!(after.position_frames, before.position_frames);
    assert_eq!(after.transport, before.transport);
    assert_eq!(after.now_playing, before.now_playing);
    assert_eq!(after.settings, before.settings);
    assert_eq!(after.error, before.error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_catalog_occurrences_conflicts_and_order_survive_restart() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut first = PlaybackClient::connect(&running).await;
    let mut second = PlaybackClient::connect(&running).await;
    assert_eq!(first.state.playlists_revision, 1);
    let (_, tracks) = index(&scratch, &mut first, &["Alpha", "Beta"]).await;
    let mut main = create(&mut first, "Zulu").await;
    second
        .reject(
            json!({"command":"create_playlist","expected_revision":1,"name":"Lost update"}),
            ErrorCode::StalePlaylistRevision,
        )
        .await;
    let accent = create(&mut first, " ÉTÉ ").await;
    assert_eq!(accent.name, "ÉTÉ");
    let literal = create(&mut first, "100%_Mix").await;
    let revision = first.state.playlists_revision;
    for name in [
        "été".to_owned(),
        " ".into(),
        "nul\0name".into(),
        "é".repeat(129),
    ] {
        first
            .reject(
                json!({"command":"create_playlist","expected_revision":revision,"name":name}),
                ErrorCode::InvalidCommand,
            )
            .await;
        assert_eq!(first.state.playlists_revision, revision);
    }
    assert_eq!(
        catalog(&mut first, "%_").await.entries,
        vec![literal.clone()]
    );
    assert_eq!(
        catalog(&mut first, "ÉTÉ").await.entries,
        vec![accent.clone()]
    );
    assert_eq!(
        catalog(&mut first, "")
            .await
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["100%_Mix", "Zulu", "ÉTÉ"]
    );
    first
        .reject(
            json!({"command":"saved_playlists","revision":1,"offset":1,"limit":1,"query":""}),
            ErrorCode::StalePlaylistRevision,
        )
        .await;
    for command in [
        json!({"command":"saved_playlists","revision":null,"offset":0,"limit":0,"query":""}),
        json!({"command":"saved_playlists","revision":null,"offset":0,"limit":501,"query":""}),
        json!({"command":"saved_playlists","revision":null,"offset":0,"limit":100,"query":"x".repeat(1025)}),
    ] {
        first.reject(command, ErrorCode::InvalidCommand).await;
    }

    let a = tracks.entries[0].id;
    let b = tracks.entries[1].id;
    main = add(&mut first, &main, tracks.revision, vec![a, b, a]).await;
    let (_, page) = entries(&mut first, main.id).await;
    assert_eq!(page.total, 3);
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        vec![a, b, a]
    );
    let ids = page
        .entries
        .iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    let other = add(&mut first, &accent, tracks.revision, vec![b]).await;
    let foreign = entries(&mut first, other.id).await.1.entries[0].id;
    for command in [
        json!({"command":"playlist_remove","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[0],foreign]}),
        json!({"command":"playlist_remove","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[0],ids[0]]}),
        json!({"command":"playlist_move","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[0]],"before":foreign}),
        json!({"command":"playlist_move","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[foreign],"before":null}),
        json!({"command":"playlist_add","playlist_id":main.id,"expected_revision":main.revision,"library_revision":tracks.revision,"track_ids":[a,999999],"before":null}),
        json!({"command":"playlist_add","playlist_id":main.id,"expected_revision":main.revision,"library_revision":tracks.revision,"track_ids":[],"before":null}),
    ] {
        first.reject(command, ErrorCode::InvalidCommand).await;
    }
    assert_eq!(entries(&mut first, main.id).await.1.entries, page.entries);
    second.reject(json!({"command":"rename_playlist","playlist_id":main.id,"expected_revision":1,"name":"Lost rename"}), ErrorCode::StalePlaylistRevision).await;
    let catalog_before = first.state.playlists_revision;
    let same = change(&mut first, json!({"command":"playlist_move","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[0]],"before":ids[0]})).await.unwrap();
    assert_eq!(same, main);
    assert_eq!(first.state.playlists_revision, catalog_before);
    main = change(&mut first, json!({"command":"playlist_move","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[0]],"before":null})).await.unwrap();
    assert_eq!(
        entries(&mut first, main.id)
            .await
            .1
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![ids[1], ids[2], ids[0]]
    );
    first.reject(json!({"command":"playlist_entries","playlist_id":main.id,"revision":page.revision,"library_revision":tracks.revision,"offset":1,"limit":1,"query":""}), ErrorCode::StalePlaylistRevision).await;
    main = change(&mut first, json!({"command":"playlist_remove","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[1]]})).await.unwrap();
    main = add(&mut first, &main, tracks.revision, vec![b]).await;
    let (_, retained) = entries(&mut first, main.id).await;
    assert!(retained.entries.last().unwrap().id > foreign);
    first.reject(json!({"command":"playlist_remove","playlist_id":main.id,"expected_revision":main.revision,"entry_ids":[ids[1]]}), ErrorCode::InvalidCommand).await;
    main = change(&mut first, json!({"command":"rename_playlist","playlist_id":main.id,"expected_revision":main.revision,"name":"zULU"})).await.unwrap();
    assert_eq!(first.state.queue_revision, 1);
    assert_eq!(first.state.queue_length, 0);
    let global = first.state.playlists_revision;
    running.stop().await;

    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.playlists_revision, global);
    assert_eq!(
        catalog(&mut restored, "zulu").await.entries,
        vec![main.clone()]
    );
    assert_eq!(
        entries(&mut restored, main.id).await.1.entries,
        retained.entries
    );
    assert!(change(&mut restored, json!({"command":"delete_playlist","playlist_id":main.id,"expected_revision":main.revision})).await.is_none());
    let replacement = create(&mut restored, "Zulu").await;
    assert!(replacement.id > literal.id);
    let replacement = add(&mut restored, &replacement, tracks.revision, vec![a]).await;
    assert!(
        entries(&mut restored, replacement.id).await.1.entries[0].id
            > retained.entries.last().unwrap().id
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_playlist_loads_preserve_queue_selection_and_pcm_intent() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (root, tracks) = index(&scratch, &mut client, &["Current", "First", "Later"]).await;
    let current = tracks
        .entries
        .iter()
        .find(|track| track.metadata.title == "Current")
        .unwrap();
    let first = tracks
        .entries
        .iter()
        .find(|track| track.metadata.title == "First")
        .unwrap();
    let later = tracks
        .entries
        .iter()
        .find(|track| track.metadata.title == "Later")
        .unwrap();
    let empty = create(&mut client, "Empty").await;
    let playlist = create(&mut client, "Load").await;
    let playlist = add(
        &mut client,
        &playlist,
        tracks.revision,
        vec![first.id, later.id],
    )
    .await;
    client.append(Path::new(&current.path)).await;
    client.apply(json!({"command":"play","entry_id":1})).await;
    client.apply(json!({"command":"pause"})).await;
    client.seek(1, 24_000).await;
    let before = client.state.clone();
    let original_queue = queue(&mut client).await;
    for (command, code) in [
        (
            load(&empty, tracks.revision, before.queue_revision, false),
            ErrorCode::InvalidCommand,
        ),
        (
            load(&playlist, tracks.revision, before.queue_revision - 1, true),
            ErrorCode::StaleRevision,
        ),
        (
            load(&playlist, tracks.revision - 1, before.queue_revision, true),
            ErrorCode::StaleLibraryRevision,
        ),
    ] {
        client.reject(command, code).await;
        unchanged_playback(&before, &client.state);
        assert_eq!(queue(&mut client).await, original_queue);
    }
    let mut stale_playlist = playlist.clone();
    stale_playlist.revision -= 1;
    client
        .reject(
            load(
                &stale_playlist,
                tracks.revision,
                before.queue_revision,
                true,
            ),
            ErrorCode::StalePlaylistRevision,
        )
        .await;
    unchanged_playback(&before, &client.state);

    // All paths are checked before commit, including later entries. Decoder
    // preparation for play is isolated from the paused current PCM/selection.
    fs::remove_file(&later.path).unwrap();
    client
        .reject(
            load(&playlist, tracks.revision, before.queue_revision, false),
            ErrorCode::InvalidCommand,
        )
        .await;
    unchanged_playback(&before, &client.state);
    assert_eq!(queue(&mut client).await, original_queue);
    tagged_wav(Path::new(&later.path), 240_000, b"Later");
    fs::write(&first.path, b"not an audio stream").unwrap();
    client
        .reject(
            load(&playlist, tracks.revision, before.queue_revision, true),
            ErrorCode::Playback,
        )
        .await;
    unchanged_playback(&before, &client.state);
    assert_eq!(queue(&mut client).await, original_queue);
    fs::remove_file(&first.path).unwrap();
    fs::hard_link(scratch.path("capture.f32"), &first.path).unwrap();
    client
        .reject(
            load(&playlist, tracks.revision, before.queue_revision, false),
            ErrorCode::InvalidCommand,
        )
        .await;
    unchanged_playback(&before, &client.state);
    fs::remove_file(&first.path).unwrap();
    tagged_wav(Path::new(&first.path), 240_000, b"First");

    let first_append = match ok(
        &mut client,
        load(&playlist, tracks.revision, before.queue_revision, false),
    )
    .await
    {
        Response::Applied {
            entry_id: Some(id), ..
        } => id,
        response => panic!("expected appended occurrence: {response:?}"),
    };
    assert_eq!(first_append, 2);
    assert_eq!(client.state.queue_revision, before.queue_revision + 1);
    assert_eq!(client.state.current_id, before.current_id);
    assert_eq!(client.state.position_frames, before.position_frames);
    assert_eq!(client.state.transport, Transport::Paused);
    assert_eq!(
        queue(&mut client)
            .await
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        vec![None, Some(first.id), Some(later.id)]
    );
    let revision = client.state.queue_revision;
    let played = match ok(
        &mut client,
        load(&playlist, tracks.revision, revision, true),
    )
    .await
    {
        Response::Applied {
            entry_id: Some(id), ..
        } => id,
        response => panic!("expected played occurrence: {response:?}"),
    };
    assert_eq!(played, 4);
    assert_eq!(client.state.queue_revision, revision + 1);
    client.selected(played, "First");
    assert!(matches!(
        client.state.transport,
        Transport::Playing | Transport::Buffering
    ));
    client.apply(json!({"command":"pause"})).await;
    client.seek(played, 24_000).await;
    let paused = client.state.clone();
    let enqueued = queue(&mut client).await;

    // Tombstoned indexed tracks remain readable with metadata and stable IDs.
    fs::remove_file(&later.path).unwrap();
    let (_, indexed) = entries(&mut client, playlist.id).await;
    assert!(
        indexed.entries[1].available,
        "entry pages read the index rather than probing the filesystem"
    );
    rescan(&mut client, root).await;
    let (library, unavailable) = entries(&mut client, playlist.id).await;
    assert!(!unavailable.entries[1].available);
    assert!(unavailable.entries[1].error.is_some());
    assert_eq!(unavailable.entries[1].metadata.title, "Later");
    assert_eq!(unavailable.revision, playlist.revision);
    client.reject(json!({"command":"playlist_entries","playlist_id":playlist.id,"revision":playlist.revision,"library_revision":tracks.revision,"offset":1,"limit":1,"query":""}), ErrorCode::StaleLibraryRevision).await;
    client
        .reject(
            load(&playlist, library, paused.queue_revision, false),
            ErrorCode::InvalidCommand,
        )
        .await;
    unchanged_playback(&paused, &client.state);
    assert_eq!(queue(&mut client).await, enqueued);
    client.reject(json!({"command":"playlist_add","playlist_id":playlist.id,"expected_revision":playlist.revision,"library_revision":library,"track_ids":[later.id],"before":null}), ErrorCode::InvalidCommand).await;
    let edited = change(&mut client, json!({"command":"playlist_remove","playlist_id":playlist.id,"expected_revision":playlist.revision,"entry_ids":[unavailable.entries[1].id]})).await.unwrap();
    unchanged_playback(&paused, &client.state);
    change(&mut client, json!({"command":"delete_playlist","playlist_id":edited.id,"expected_revision":edited.revision})).await;
    unchanged_playback(&paused, &client.state);
    assert_eq!(queue(&mut client).await, enqueued);
    running.stop().await;
    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(queue(&mut restored).await, enqueued);
    assert_eq!(restored.state.queue_revision, paused.queue_revision);
    restored.selected(played, "First");
    assert_eq!(restored.state.position_frames, 24_000);
    assert_eq!(restored.state.transport, Transport::Paused);
    assert!(catalog(&mut restored, "Load").await.entries.is_empty());
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_playlist_enqueues_once_while_entry_browsing_stays_revision_bound() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (root, tracks) = index(&scratch, &mut client, &["Repeated"]).await;
    let playlist = create(&mut client, "Long").await;
    client.reject(json!({"command":"playlist_add","playlist_id":playlist.id,"expected_revision":playlist.revision,"library_revision":tracks.revision,"track_ids":vec![tracks.entries[0].id; 501],"before":null}), ErrorCode::InvalidCommand).await;
    let playlist = add(
        &mut client,
        &playlist,
        tracks.revision,
        vec![tracks.entries[0].id; 500],
    )
    .await;
    let playlist = add(
        &mut client,
        &playlist,
        tracks.revision,
        vec![tracks.entries[0].id; 5],
    )
    .await;
    assert_eq!(playlist.entry_count, Some(505));
    let (library, page) = entries(&mut client, playlist.id).await;
    assert_eq!(page.entries.len(), 500);
    assert_eq!(page.total, 505);
    client.reject(json!({"command":"playlist_entries","playlist_id":playlist.id,"revision":playlist.revision,"library_revision":null,"offset":500,"limit":100,"query":""}), ErrorCode::InvalidCommand).await;
    let last = match ok(&mut client, json!({"command":"playlist_entries","playlist_id":playlist.id,"revision":playlist.revision,"library_revision":library,"offset":500,"limit":100,"query":""})).await {
        Response::PlaylistEntries { page, .. } => page,
        response => panic!("expected final page: {response:?}"),
    };
    assert_eq!(last.entries.len(), 5);
    assert!(last.entries[0].id > page.entries[499].id);
    tagged_wav(
        Path::new(&tracks.entries[0].path),
        240_000,
        b"Updated metadata",
    );
    rescan(&mut client, root).await;
    client.reject(json!({"command":"playlist_entries","playlist_id":playlist.id,"revision":playlist.revision,"library_revision":library,"offset":500,"limit":100,"query":""}), ErrorCode::StaleLibraryRevision).await;
    let (library, updated) = entries(&mut client, playlist.id).await;
    assert_eq!(updated.revision, playlist.revision);
    assert_eq!(updated.entries[0].id, page.entries[0].id);
    assert_eq!(updated.entries[0].metadata.title, "Updated metadata");
    let queue_revision = client.state.queue_revision;
    ok(&mut client, load(&playlist, library, queue_revision, false)).await;
    assert_eq!(client.state.queue_length, 505);
    assert_eq!(client.state.queue_revision, queue_revision + 1);
    assert_eq!(client.state.transport, Transport::Stopped);
    match ok(
        &mut client,
        json!({"command":"queue_page","revision":queue_revision+1,"offset":500,"limit":100}),
    )
    .await
    {
        Response::QueuePage { total, entries, .. } => {
            assert_eq!(total, 505);
            assert_eq!(
                entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
                vec![501, 502, 503, 504, 505]
            );
        }
        response => panic!("expected appended tail: {response:?}"),
    }
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structured_filters_share_wire_errors_and_do_not_narrow_playlist_enqueue() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (_, tracks) = index(&scratch, &mut client, &["Alpha", "Beta"]).await;
    let alpha = tracks
        .entries
        .iter()
        .find(|track| track.metadata.title == "Alpha")
        .unwrap()
        .id;
    let beta = tracks
        .entries
        .iter()
        .find(|track| track.metadata.title == "Beta")
        .unwrap()
        .id;
    let playlist = create(&mut client, "Filtered").await;
    let playlist = add(
        &mut client,
        &playlist,
        tracks.revision,
        vec![alpha, beta, alpha],
    )
    .await;
    let (_, all) = entries(&mut client, playlist.id).await;
    let filter = "(title:[Alpha,Missing] | title:Beta) & !title:Beta";
    let library_page = match ok(
        &mut client,
        json!({
            "command":"library_tracks","revision":tracks.revision,"offset":0,"limit":1,
            "query":filter,"album_id":null
        }),
    )
    .await
    {
        Response::LibraryTracks { page } => page,
        response => panic!("expected filtered tracks: {response:?}"),
    };
    assert_eq!(library_page.total, 1);
    assert_eq!(library_page.entries[0].id, alpha);
    for (offset, expected) in [(0, all.entries[0].id), (1, all.entries[2].id)] {
        let filtered = match ok(
            &mut client,
            json!({
                "command":"playlist_entries","playlist_id":playlist.id,"revision":playlist.revision,
                "library_revision":tracks.revision,"offset":offset,"limit":1,"query":filter
            }),
        )
        .await
        {
            Response::PlaylistEntries { page, .. } => page,
            response => panic!("expected filtered entries: {response:?}"),
        };
        assert_eq!((filtered.total, filtered.offset), (2, offset));
        assert_eq!(
            filtered
                .entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [expected]
        );
    }
    let before = client.state.clone();
    for command in [
        "library_artists",
        "library_albums",
        "library_tracks",
        "playlist_entries",
    ] {
        for filter in [
            "artist:[]".to_owned(),
            "x".repeat(1025),
            format!("{}title:Alpha", "!".repeat(33)),
        ] {
            let mut request = json!({
                "command":command,"revision":null,"offset":0,"limit":1,"query":filter
            });
            if command == "playlist_entries" {
                request["playlist_id"] = json!(playlist.id);
                request["library_revision"] = Value::Null;
            }
            match client.request(request).await {
                Reply::Error {
                    code,
                    message,
                    queue_revision,
                } => {
                    assert_eq!(code, ErrorCode::InvalidFilter);
                    assert!(message.contains("UTF-8 byte "), "{message}");
                    assert_eq!(queue_revision, before.queue_revision);
                }
                reply => panic!("expected typed filter error: {reply:?}"),
            }
        }
    }
    unchanged_playback(&before, &client.state);
    ok(
        &mut client,
        load(&playlist, tracks.revision, before.queue_revision, false),
    )
    .await;
    let queued = queue(&mut client).await;
    assert_eq!(
        queued
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        [Some(alpha), Some(beta), Some(alpha)]
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_batch_commands_preserve_selection_and_occurrence_identity_through_restart() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (_, tracks) = index(&scratch, &mut client, &["Alpha", "Beta"]).await;
    let a = tracks.entries[0].id;
    let b = tracks.entries[1].id;
    let revision = client.state.queue_revision;
    match ok(
        &mut client,
        json!({"command":"insert_tracks","expected_revision":revision,
        "library_revision":tracks.revision,"track_ids":[a,b,a,b,a],"before":null}),
    )
    .await
    {
        Response::Applied { entry_id, .. } => assert_eq!(entry_id, Some(1)),
        response => panic!("expected inserted queue: {response:?}"),
    }
    client.apply(json!({"command":"play","entry_id":3})).await;
    client.apply(json!({"command":"pause"})).await;
    client.seek(3, 24_000).await;
    let paused = client.state.clone();
    client
        .apply(
            json!({"command":"move","expected_revision":paused.queue_revision,
        "entry_ids":[4,1,3],"before":2}),
        )
        .await;
    assert_eq!(
        queue(&mut client)
            .await
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [1, 3, 4, 2, 5]
    );
    assert_eq!(client.state.queue_revision, paused.queue_revision + 1);
    assert_eq!(client.state.current_id, Some(3));
    assert_eq!(client.state.position_frames, 24_000);
    assert_eq!(client.state.transport, Transport::Paused);
    let before = client.state.clone();
    for anchor in [Some(3), Some(2)] {
        client
            .apply(
                json!({"command":"move","expected_revision":before.queue_revision,
            "entry_ids":[4,3,1],"before":anchor}),
            )
            .await;
        unchanged_playback(&before, &client.state);
    }
    let original = queue(&mut client).await;
    for (command, code) in [
        (
            json!({"command":"move","expected_revision":before.queue_revision,"entry_ids":[1,999],"before":1}),
            ErrorCode::InvalidCommand,
        ),
        (
            json!({"command":"move","expected_revision":before.queue_revision,"entry_ids":[1,1],"before":null}),
            ErrorCode::InvalidCommand,
        ),
        (
            json!({"command":"move","expected_revision":before.queue_revision,"entry_ids":[1],"before":999}),
            ErrorCode::InvalidCommand,
        ),
        (
            json!({"command":"move","expected_revision":before.queue_revision-1,"entry_ids":[1],"before":null}),
            ErrorCode::StaleRevision,
        ),
        (
            json!({"command":"insert_tracks","expected_revision":before.queue_revision,"library_revision":tracks.revision-1,"track_ids":[a],"before":2}),
            ErrorCode::StaleLibraryRevision,
        ),
        (
            json!({"command":"insert_tracks","expected_revision":before.queue_revision,"library_revision":tracks.revision,"track_ids":[a,999],"before":2}),
            ErrorCode::InvalidCommand,
        ),
        (
            json!({"command":"insert_tracks","expected_revision":before.queue_revision,"library_revision":tracks.revision,"track_ids":[a],"before":999}),
            ErrorCode::InvalidCommand,
        ),
    ] {
        client.reject(command, code).await;
        unchanged_playback(&before, &client.state);
        assert_eq!(queue(&mut client).await, original);
    }
    client
        .apply(
            json!({"command":"insert_tracks","expected_revision":before.queue_revision,
        "library_revision":tracks.revision,"track_ids":[b,b],"before":2}),
        )
        .await;
    let persisted = queue(&mut client).await;
    assert_eq!(
        persisted.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        [1, 3, 4, 6, 7, 2, 5]
    );
    assert_eq!(persisted[3].track_id, Some(b));
    assert_eq!(persisted[4].track_id, Some(b));
    assert_eq!(client.state.current_id, Some(3));
    assert_eq!(client.state.position_frames, 24_000);
    let playlist = create(&mut client, "Copy at anchor").await;
    let playlist = add(&mut client, &playlist, tracks.revision, vec![a, b, a]).await;
    let old = entries(&mut client, playlist.id).await.1;
    let playlist = change(
        &mut client,
        json!({"command":"playlist_add","playlist_id":playlist.id,
        "expected_revision":playlist.revision,"library_revision":tracks.revision,"track_ids":[b,b],
        "before":old.entries[1].id}),
    )
    .await
    .unwrap();
    let copied = entries(&mut client, playlist.id).await.1;
    assert_eq!(
        copied
            .entries
            .iter()
            .map(|entry| entry.track_id)
            .collect::<Vec<_>>(),
        [a, b, b, b, a]
    );
    let playlist = change(&mut client, json!({"command":"playlist_move","playlist_id":playlist.id,
        "expected_revision":playlist.revision,"entry_ids":[copied.entries[4].id,copied.entries[1].id],
        "before":copied.entries[0].id})).await.unwrap();
    let saved = entries(&mut client, playlist.id).await.1;
    assert_eq!(
        saved
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [
            copied.entries[1].id,
            copied.entries[4].id,
            copied.entries[0].id,
            copied.entries[2].id,
            copied.entries[3].id
        ]
    );
    // Indexed availability alone is insufficient: every copied source is
    // checked on disk, including capture aliases in a later batch member.
    let source = Path::new(&tracks.entries[1].path);
    let source_bytes = fs::read(source).unwrap();
    let before_failure = client.state.clone();
    for capture_alias in [false, true] {
        fs::remove_file(source).unwrap();
        if capture_alias {
            fs::hard_link(scratch.path("capture.f32"), source).unwrap();
        }
        client
            .reject(
                json!({"command":"insert_tracks","expected_revision":before_failure.queue_revision,
            "library_revision":tracks.revision,"track_ids":[a,b],"before":2}),
                ErrorCode::InvalidCommand,
            )
            .await;
        client.reject(json!({"command":"playlist_add","playlist_id":playlist.id,"expected_revision":playlist.revision,
            "library_revision":tracks.revision,"track_ids":[a,b],"before":null}), ErrorCode::InvalidCommand).await;
        unchanged_playback(&before_failure, &client.state);
        assert_eq!(queue(&mut client).await, persisted);
        assert_eq!(
            entries(&mut client, playlist.id).await.1.entries,
            saved.entries
        );
        if capture_alias {
            fs::remove_file(source).unwrap();
        }
        fs::write(source, &source_bytes).unwrap();
    }
    let final_revision = client.state.queue_revision;
    running.stop().await;
    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(queue(&mut restored).await, persisted);
    assert_eq!(restored.state.queue_revision, final_revision);
    assert_eq!(restored.state.current_id, Some(3));
    assert_eq!(restored.state.position_frames, 24_000);
    assert_eq!(restored.state.transport, Transport::Paused);
    assert_eq!(
        entries(&mut restored, playlist.id).await.1.entries,
        saved.entries
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smart_rules_snapshots_and_library_changes_are_isolated_from_the_queue() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let (root, tracks) = index(&scratch, &mut client, &["Alpha", "Beta"]).await;
    let smart = change(
        &mut client,
        json!({
            "command":"create_smart_playlist","expected_revision":1,"name":"Live",
            "query":"title:[Alpha,Beta] & !title:Beta"
        }),
    )
    .await
    .unwrap();
    assert_eq!(smart.kind, tyranny::protocol::PlaylistKind::Smart);
    assert_eq!(smart.entry_count, None);
    let (library, live) = entries(&mut client, smart.id).await;
    assert_eq!(live.total, 1);
    assert_eq!(live.entries[0].metadata.title, "Alpha");
    assert_eq!(live.entries[0].id, live.entries[0].track_id);
    client.reject(json!({"command":"update_smart_playlist","playlist_id":smart.id,"expected_revision":1,"query":"artist:[]"}), ErrorCode::InvalidFilter).await;
    client.reject(json!({"command":"create_smart_playlist","expected_revision":2,"name":"Broken","query":"("}), ErrorCode::InvalidFilter).await;
    for command in [
        json!({"command":"playlist_add","playlist_id":smart.id,"expected_revision":1,"library_revision":library,"track_ids":[live.entries[0].track_id],"before":null}),
        json!({"command":"playlist_remove","playlist_id":smart.id,"expected_revision":1,"entry_ids":[]}),
        json!({"command":"playlist_move","playlist_id":smart.id,"expected_revision":1,"entry_ids":[live.entries[0].id],"before":live.entries[0].id}),
    ] {
        client.reject(command, ErrorCode::InvalidCommand).await;
    }
    let filtered = ok(&mut client, json!({"command":"playlist_entries","playlist_id":smart.id,"revision":1,"library_revision":library,"offset":0,"limit":1,"query":"title:Beta"})).await;
    assert!(matches!(filtered, Response::PlaylistEntries { page, .. } if page.total == 0));
    let frozen = change(&mut client, json!({"command":"snapshot_smart_playlist","playlist_id":smart.id,"playlist_revision":1,"library_revision":library,"expected_revision":2,"name":"Frozen"})).await.unwrap();
    assert_eq!(frozen.kind, tyranny::protocol::PlaylistKind::Regular);
    assert_eq!(frozen.entry_count, Some(1));
    assert_eq!(frozen.query, None);
    match ok(&mut client, json!({"command":"saved_playlists","revision":null,"offset":0,"limit":1,"query":"","kind":"regular"})).await {
        Response::SavedPlaylists { page } => {
            assert_eq!(page.total, 1);
            assert_eq!(page.entries, vec![frozen.clone()]);
        }
        response => panic!("expected regular catalog: {response:?}"),
    }
    client.reject(json!({"command":"snapshot_smart_playlist","playlist_id":smart.id,"playlist_revision":1,"library_revision":library,"expected_revision":2,"name":"Stale"}), ErrorCode::StalePlaylistRevision).await;
    let queue_revision = client.state.queue_revision;
    ok(&mut client, load(&smart, library, queue_revision, false)).await;
    let queued = queue(&mut client).await;
    assert_eq!(queued.len(), 1);
    let playback = client.state.clone();
    let edited = change(&mut client, json!({"command":"update_smart_playlist","playlist_id":smart.id,"expected_revision":1,"query":"title:[Alpha,Gamma]"})).await.unwrap();
    unchanged_playback(&playback, &client.state);
    client.reject(json!({"command":"playlist_entries","playlist_id":smart.id,"revision":1,"library_revision":library,"offset":0,"limit":1,"query":""}), ErrorCode::StalePlaylistRevision).await;
    tagged_wav(&scratch.path("music/Gamma.wav"), 240_000, b"Gamma");
    rescan(&mut client, root).await;
    let (new_library, updated) = entries(&mut client, smart.id).await;
    assert_eq!(updated.total, 2);
    assert_eq!(updated.revision, edited.revision);
    assert_eq!(entries(&mut client, frozen.id).await.1.total, 1);
    assert_eq!(queue(&mut client).await, queued);
    unchanged_playback(&playback, &client.state);
    client.reject(json!({"command":"snapshot_smart_playlist","playlist_id":smart.id,"playlist_revision":2,"library_revision":library,"expected_revision":4,"name":"Old library"}), ErrorCode::StaleLibraryRevision).await;
    // Filesystem preparation failure cannot partially append either result.
    let gamma = updated
        .entries
        .iter()
        .find(|entry| entry.metadata.title == "Gamma")
        .unwrap();
    fs::remove_file(&gamma.path).unwrap();
    client
        .reject(
            load(&edited, new_library, playback.queue_revision, true),
            ErrorCode::InvalidCommand,
        )
        .await;
    assert_eq!(queue(&mut client).await, queued);
    unchanged_playback(&playback, &client.state);
    assert_eq!(catalog(&mut client, "").await.total, 2);
    assert_eq!(tracks.total, 2);
    running.stop().await;
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let restored = catalog(&mut client, "Live").await.entries.remove(0);
    assert_eq!(restored, edited);
    assert_eq!(entries(&mut client, frozen.id).await.1.total, 1);
    assert_eq!(queue(&mut client).await, queued);
    let renamed = change(&mut client, json!({"command":"rename_playlist","playlist_id":restored.id,"expected_revision":restored.revision,"name":"Renamed live"})).await.unwrap();
    assert_eq!(renamed.query, edited.query);
    assert_eq!(renamed.kind, tyranny::protocol::PlaylistKind::Smart);
    change(&mut client, json!({"command":"delete_playlist","playlist_id":renamed.id,"expected_revision":renamed.revision})).await;
    assert_eq!(entries(&mut client, frozen.id).await.1.total, 1);
    assert_eq!(queue(&mut client).await, queued);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smart_server_snapshots_and_enqueues_all_505_tracks_without_client_hydration() {
    let scratch = Scratch::new();
    let music = scratch.path("music");
    fs::create_dir(&music).unwrap();
    for index in 0..505 {
        tagged_wav(
            &music.join(format!("{index:04}.wav")),
            480,
            format!("Match {index:04}").as_bytes(),
        );
    }
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    ok(&mut client, json!({"command":"add_root","path":music})).await;
    client
        .until(|state| state.library.scan.phase == ScanPhase::Complete)
        .await;
    let smart = change(&mut client, json!({"command":"create_smart_playlist","expected_revision":1,"name":"All matches","query":"title:Match"})).await.unwrap();
    let (library, first) = entries(&mut client, smart.id).await;
    assert_eq!((first.total, first.entries.len()), (505, 500));
    client.reject(json!({"command":"playlist_entries","playlist_id":smart.id,"revision":1,"library_revision":null,"offset":500,"limit":100,"query":""}), ErrorCode::InvalidCommand).await;
    let tail = match ok(&mut client, json!({"command":"playlist_entries","playlist_id":smart.id,"revision":1,"library_revision":library,"offset":500,"limit":100,"query":""})).await {
        Response::PlaylistEntries { page, .. } => page,
        response => panic!("expected smart tail: {response:?}"),
    };
    assert_eq!(tail.entries.len(), 5);
    assert_eq!(tail.entries[4].metadata.title, "Match 0504");
    let frozen = change(&mut client, json!({"command":"snapshot_smart_playlist","playlist_id":smart.id,"playlist_revision":1,"library_revision":library,"expected_revision":2,"name":"All frozen"})).await.unwrap();
    assert_eq!(frozen.entry_count, Some(505));
    let queue_revision = client.state.queue_revision;
    ok(&mut client, load(&smart, library, queue_revision, false)).await;
    assert_eq!(client.state.queue_length, 505);
    assert_eq!(client.state.queue_revision, queue_revision + 1);
    match ok(
        &mut client,
        json!({"command":"queue_page","revision":queue_revision+1,"offset":500,"limit":100}),
    )
    .await
    {
        Response::QueuePage { total, entries, .. } => {
            assert_eq!(total, 505);
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.track_id)
                    .collect::<Vec<_>>(),
                tail.entries
                    .iter()
                    .map(|entry| Some(entry.track_id))
                    .collect::<Vec<_>>()
            );
        }
        response => panic!("expected queue tail: {response:?}"),
    }
    running.stop().await;
}
