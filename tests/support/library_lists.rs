//! v9 list queries through a real authenticated client and native indexing.
use super::{
    saved_playlists::{add, create, index, ok},
    *,
};
use serde_json::{Value, json};
use tyranny::listing::Grouping;

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
