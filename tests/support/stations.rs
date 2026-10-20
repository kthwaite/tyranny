//! Saved radio stations through real authenticated clients and the clocked
//! capture owner. No direct command dispatch.
use super::*;
use serde_json::{Value, json};
use tyranny::protocol::Station;

async fn ok(client: &mut PlaybackClient, command: Value) -> Response {
    match client.request(command).await {
        Reply::Ok { result } => {
            match &result {
                Response::Snapshot { snapshot }
                | Response::Applied { snapshot, .. }
                | Response::PlaylistApplied { snapshot, .. }
                | Response::StationsApplied { snapshot, .. } => {
                    assert!(snapshot.sequence >= client.state.sequence);
                    client.state = snapshot.as_ref().clone();
                }
                _ => {}
            }
            result
        }
        reply => panic!("station request failed: {reply:?}"),
    }
}
async fn catalog(client: &mut PlaybackClient) -> (u64, Vec<Station>) {
    match ok(client, json!({"command":"stations"})).await {
        Response::Stations { revision, stations } => (revision, stations),
        response => panic!("expected stations: {response:?}"),
    }
}
/// Runs a station edit. A second client, which only listens, must see the
/// broadcast state carry the same `stations_revision` as the reply snapshot.
async fn edit(
    client: &mut PlaybackClient,
    observer: &mut PlaybackClient,
    command: Value,
) -> Vec<Station> {
    let Response::StationsApplied { snapshot, stations } = ok(client, command).await else {
        panic!("expected a stations change");
    };
    let revision = snapshot.stations_revision;
    observer
        .until(|state| state.stations_revision == revision)
        .await;
    assert_eq!(observer.state.stations_revision, revision);
    // The reply carries the very state that was broadcast, not a later one.
    assert_eq!(observer.state.sequence, snapshot.sequence);
    stations
}
fn add_command(client: &PlaybackClient, stations: Value) -> Value {
    json!({"command":"add_stations","expected_revision":client.state.stations_revision,"stations":stations})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn station_catalog_round_trips_and_survives_restart() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let mut observer = PlaybackClient::connect(&running).await;
    assert_eq!(client.state.stations_revision, 0);
    assert_eq!(catalog(&mut client).await, (0, vec![]));

    let command = add_command(
        &client,
        json!([
            {"name":"Zeta FM","url":"https://zeta.example/live"},
            {"name":"","url":"https://alpha.example/live"},
        ]),
    );
    let added = edit(&mut client, &mut observer, command).await;
    assert_eq!(added.len(), 2);
    assert_eq!(client.state.stations_revision, 1);
    let (revision, listed) = catalog(&mut client).await;
    assert_eq!(revision, 1);
    assert_eq!(
        listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["alpha.example", "Zeta FM"]
    );

    let zeta = listed[1].id;
    let updated = edit(
        &mut client,
        &mut observer,
        json!({"command":"update_station","station_id":zeta,"expected_revision":1,
            "name":"Zeta Radio","url":"https://zeta.example/other"}),
    )
    .await;
    assert_eq!(updated[0].name, "Zeta Radio");
    assert_eq!(client.state.stations_revision, 2);

    let alpha = listed[0].id;
    let removed = edit(
        &mut client,
        &mut observer,
        json!({"command":"remove_station","station_id":alpha,"expected_revision":2}),
    )
    .await;
    assert!(removed.is_empty());
    assert_eq!(client.state.stations_revision, 3);
    let expected = catalog(&mut client).await;
    assert_eq!(expected.1, updated);
    drop(client);
    running.stop().await;

    let running = Running::start(&scratch).await;
    let mut restored = PlaybackClient::connect(&running).await;
    assert_eq!(restored.state.stations_revision, 3);
    assert_eq!(catalog(&mut restored).await, expected);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_station_revision_is_rejected() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let mut observer = PlaybackClient::connect(&running).await;
    let command = add_command(&client, json!([{"name":"A","url":"https://a.example/"}]));
    edit(&mut client, &mut observer, command).await;
    let reply = client
        .request(json!({"command":"add_stations","expected_revision":0,
            "stations":[{"name":"B","url":"https://b.example/"}]}))
        .await;
    match reply {
        Reply::Error { code, .. } => assert_eq!(code, ErrorCode::StaleStationsRevision),
        reply => panic!("stale edit succeeded: {reply:?}"),
    }
    assert_eq!(
        serde_json::to_value(ErrorCode::StaleStationsRevision).unwrap(),
        "stale_stations_revision"
    );
    assert_eq!(catalog(&mut client).await.1.len(), 1);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_station_batch_commits_nothing() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let command = add_command(
        &client,
        json!([
            {"name":"One","url":"https://one.example/"},
            {"name":"Bad","url":"ftp://x"},
            {"name":"Three","url":"https://three.example/"},
        ]),
    );
    match client.request(command).await {
        Reply::Error { code, message, .. } => {
            assert_eq!(code, ErrorCode::InvalidCommand);
            assert!(message.contains('1'), "{message}");
        }
        reply => panic!("invalid batch succeeded: {reply:?}"),
    }
    assert_eq!(catalog(&mut client).await, (0, vec![]));
    assert_eq!(client.state.stations_revision, 0);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn station_edits_do_not_touch_queue_entries() {
    let scratch = Scratch::new();
    let running = Running::start(&scratch).await;
    let mut client = PlaybackClient::connect(&running).await;
    let mut observer = PlaybackClient::connect(&running).await;
    let (url, name) = ("https://radio.example/live", "Radio One");
    let command = add_command(&client, json!([{"name":name,"url":url}]));
    let station = edit(&mut client, &mut observer, command).await.remove(0);
    let queue_revision = client.state.queue_revision;
    ok(
        &mut client,
        json!({"command":"append_stream","expected_revision":queue_revision,
            "url":station.url,"title":station.name}),
    )
    .await;
    let original = saved_playlists::queue(&mut client).await;
    assert_eq!(original.len(), 1);
    let stream = original[0].stream.clone().unwrap();
    assert_eq!((stream.url.as_str(), stream.title.as_str()), (url, name));

    let revision = client.state.stations_revision;
    edit(
        &mut client,
        &mut observer,
        json!({"command":"update_station","station_id":station.id,
            "expected_revision":revision,
            "name":"Renamed","url":"https://elsewhere.example/live"}),
    )
    .await;
    let revision = client.state.stations_revision;
    edit(
        &mut client,
        &mut observer,
        json!({"command":"remove_station","station_id":station.id,
            "expected_revision":revision}),
    )
    .await;
    let after = saved_playlists::queue(&mut client).await;
    assert_eq!(after[0].stream.as_ref().unwrap(), &stream);
    running.stop().await;
}
