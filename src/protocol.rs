//! Version 9 local control protocol. Queue/library/playlist pages are revision-bound; reconnect
//! replaces cached control state. A missing reply never authorizes command replay.
use crate::library_types::{
    Album, AlbumArtist, AlbumSort, AlbumSummary, GroupSpan, LibraryReply, LibraryState, ListRow,
    ListTotals, NamedCount, Page, Root, Track, TrackMetadata,
};
use crate::listing::ListDescription;
use crate::{decoder::GainMode, play_order::RepeatScope};
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;

pub const VERSION: u32 = 9;
pub const MAX_PAGE_SIZE: usize = 500;
/// The most tracks one enqueue or playlist add may resolve to.
pub const MAX_RUN_ENTRIES: usize = 100_000;
/// The most ranges one list selection may name.
pub const MAX_SELECTION_RANGES: usize = 64;
/// Played entries the queue keeps, not counting the current entry.
pub const PLAYED_RETENTION: usize = 100;

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
}

/// Serde treats a missing `Option` field as `None`; v9 list commands require
/// every field, so their optional fields decode through this.
fn required<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Snapshot,
    Artwork {
        album_id: Option<u64>,
        entry_id: Option<u64>,
        library_revision: u64,
        size: ArtworkSize,
    },
    QueuePage {
        revision: Option<u64>,
        offset: usize,
        limit: usize,
    },
    Append {
        expected_revision: u64,
        path: String,
    },
    Remove {
        expected_revision: u64,
        entry_id: u64,
    },
    Move {
        expected_revision: u64,
        entry_ids: Vec<u64>,
        before: Option<u64>,
    },
    Clear {
        expected_revision: u64,
    },
    /// Removes the entries after the current one, which keeps playing. With
    /// nothing current, removes every unplayed entry.
    ClearUpcoming {
        expected_revision: u64,
    },
    /// Puts the upcoming entries (as `ClearUpcoming` defines them) in a uniform
    /// random order. Other entries keep their places.
    ShuffleUpcoming {
        expected_revision: u64,
    },
    /// Creates a regular playlist of the upcoming entries that have a library track.
    SaveUpcoming {
        expected_revision: u64,
        expected_playlists_revision: u64,
        name: String,
    },
    Play {
        entry_id: Option<u64>,
    },
    Pause,
    TogglePlayPause,
    Next,
    Previous,
    Stop,
    Seek {
        entry_id: u64,
        position_frames: u64,
    },
    SetGain {
        mode: GainMode,
    },
    SetResume {
        enabled: bool,
    },
    AddRoot {
        path: String,
    },
    Rescan {
        root_id: u64,
    },
    LibraryRoots {
        revision: Option<u64>,
        offset: usize,
        limit: usize,
        query: String,
    },
    LibraryArtists {
        revision: Option<u64>,
        offset: usize,
        limit: usize,
        query: String,
    },
    LibraryAlbums {
        revision: Option<u64>,
        offset: usize,
        limit: usize,
        query: String,
        artist_id: Option<u64>,
    },
    LibraryTracks {
        revision: Option<u64>,
        offset: usize,
        limit: usize,
        query: String,
        album_id: Option<u64>,
    },
    ListTracks {
        list: ListDescription,
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
        offset: usize,
        limit: usize,
    },
    ListAlbums {
        list: ListDescription,
        sort: AlbumSort,
        descending: bool,
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
        offset: usize,
        limit: usize,
    },
    ListArtists {
        query: String,
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
        offset: usize,
        limit: usize,
    },
    ListGenres {
        query: String,
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
        offset: usize,
        limit: usize,
    },
    AlbumSummaries {
        album_ids: Vec<u64>,
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
    },
    ListGroups {
        list: ListDescription,
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
    },
    AppendTrack {
        expected_revision: u64,
        library_revision: u64,
        track_id: u64,
    },
    InsertTracks {
        expected_revision: u64,
        library_revision: u64,
        track_ids: Vec<u64>,
        before: Option<u64>,
    },
    SavedPlaylists {
        revision: Option<u64>,
        offset: usize,
        limit: usize,
        query: String,
        kind: Option<PlaylistKind>,
    },
    PlaylistEntries {
        playlist_id: u64,
        revision: Option<u64>,
        library_revision: Option<u64>,
        offset: usize,
        limit: usize,
        query: String,
    },
    CreatePlaylist {
        expected_revision: u64,
        name: String,
    },
    CreateSmartPlaylist {
        expected_revision: u64,
        name: String,
        query: String,
    },
    UpdateSmartPlaylist {
        playlist_id: u64,
        expected_revision: u64,
        query: String,
    },
    SnapshotSmartPlaylist {
        playlist_id: u64,
        playlist_revision: u64,
        library_revision: u64,
        expected_revision: u64,
        name: String,
    },
    RenamePlaylist {
        playlist_id: u64,
        expected_revision: u64,
        name: String,
    },
    DeletePlaylist {
        playlist_id: u64,
        expected_revision: u64,
    },
    PlaylistAdd {
        playlist_id: u64,
        expected_revision: u64,
        library_revision: u64,
        track_ids: Vec<u64>,
        before: Option<u64>,
    },
    /// Adds every row of `selection` (up to `MAX_RUN_ENTRIES`) in one edit.
    PlaylistAddSelection {
        playlist_id: u64,
        expected_revision: u64,
        selection: TrackSelection,
        before: Option<u64>,
    },
    PlaylistRemove {
        playlist_id: u64,
        expected_revision: u64,
        entry_ids: Vec<u64>,
    },
    PlaylistMove {
        playlist_id: u64,
        expected_revision: u64,
        entry_ids: Vec<u64>,
        before: Option<u64>,
    },
    EnqueuePlaylist {
        playlist_id: u64,
        playlist_revision: u64,
        library_revision: u64,
        expected_revision: u64,
        play: bool,
    },
    EnqueueTracks {
        selection: TrackSelection,
        expected_revision: u64,
        placement: Placement,
        play: bool,
        source: QueueSource,
    },
}

/// Tracks named by a client, either directly or as rows of a list.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrackSelection {
    /// 1..=500 track IDs, in order.
    Ids {
        library_revision: u64,
        track_ids: Vec<u64>,
    },
    /// Rows of `list`. `ranges` are half-open 0-based row positions, sorted and
    /// disjoint, 1..=64 of them; an end of `u64::MAX` means the end of the list.
    /// `excluded` names at most 500 row IDs. `first_row_id`, when set, must be
    /// the row at `ranges[0][0]`.
    List {
        list: ListDescription,
        library_revision: u64,
        ranges: Vec<[u64; 2]>,
        excluded: Vec<u64>,
        #[serde(deserialize_with = "required")]
        first_row_id: Option<u64>,
    },
}

/// Where an enqueue puts its new entries.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// After the last entry.
    Append,
    /// Straight after the current entry, or last when nothing is current.
    Next,
    /// After removing every entry after the current one. With nothing current,
    /// every unplayed entry is removed.
    ReplaceUpcoming,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkSize {
    Thumbnail,
    Display,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkStatus {
    Ready,
    Missing,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkSource {
    Embedded,
    Folder,
    CoverArtArchive,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Artwork {
    pub status: ArtworkStatus,
    pub source: Option<ArtworkSource>,
    pub cache_key: Option<String>,
    pub width: u32,
    pub height: u32,
    pub mime_type: Option<String>,
    pub data: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Settings {
    pub gain_mode: GainMode,
    pub resume_on_start: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            gain_mode: GainMode::Off,
            resume_on_start: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Stopped,
    Paused,
    Buffering,
    Playing,
    Ended,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct NowPlaying {
    pub entry_id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_frames: Option<u64>,
    pub metadata_error: Option<String>,
    /// Indexed track behind the entry, if any. The owner fills this and
    /// `album_id`/`format` from the library; the player leaves them blank.
    pub track_id: Option<u64>,
    pub album_id: Option<u64>,
    pub format: String,
    /// Gain the decoder applied; `None` when gain mode is off.
    pub gain_db: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ControlState {
    pub sequence: u64,
    pub queue_revision: u64,
    pub playlists_revision: u64,
    pub queue_length: usize,
    pub current_id: Option<u64>,
    pub can_next: bool,
    pub can_previous: bool,
    pub now_playing: Option<Arc<NowPlaying>>,
    pub position_frames: u64,
    pub sample_rate: u32,
    pub transport: Transport,
    pub settings: Settings,
    pub error: Option<String>,
    pub underruns: u64,
    pub library: LibraryState,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct QueueItem {
    pub id: u64,
    pub path: String,
    pub track_id: Option<u64>,
    pub source: Option<QueueSource>,
    pub run_id: Option<u64>,
    pub played: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<u64>,
    pub duration_ms: Option<u64>,
}

/// Longest `QueueSource::name`, in UTF-8 bytes.
pub const MAX_SOURCE_NAME_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Album,
    Playlist,
    Smart,
    Songs,
    Genre,
    Artist,
    Recent,
    Compilations,
    Drag,
    Direct,
}
impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Playlist => "playlist",
            Self::Smart => "smart",
            Self::Songs => "songs",
            Self::Genre => "genre",
            Self::Artist => "artist",
            Self::Recent => "recent",
            Self::Compilations => "compilations",
            Self::Drag => "drag",
            Self::Direct => "direct",
        }
    }
    /// `None` for a name this server does not know.
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Album,
            Self::Playlist,
            Self::Smart,
            Self::Songs,
            Self::Genre,
            Self::Artist,
            Self::Recent,
            Self::Compilations,
            Self::Drag,
            Self::Direct,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == name)
    }
    /// Albums and playlists repeat as a run. Every other source repeats the queue.
    pub fn repeat_scope(self) -> RepeatScope {
        match self {
            Self::Album | Self::Playlist | Self::Smart => RepeatScope::Run,
            _ => RepeatScope::Queue,
        }
    }
}

/// Where a queue entry came from, as the Up Next view shows it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct QueueSource {
    pub kind: SourceKind,
    /// At most `MAX_SOURCE_NAME_BYTES` bytes.
    pub name: String,
}
impl QueueSource {
    /// Server-side stamp. A long name is cut on a character boundary.
    pub fn stamped(kind: SourceKind, name: &str) -> Self {
        let mut end = name.len().min(MAX_SOURCE_NAME_BYTES);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            kind,
            name: name[..end].to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistKind {
    Regular,
    Smart,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    pub revision: u64,
    pub kind: PlaylistKind,
    pub query: Option<String>,
    pub entry_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PlaylistEntry {
    pub id: u64,
    pub track_id: u64,
    pub path: String,
    pub metadata: TrackMetadata,
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Artwork {
        artwork: Artwork,
    },
    Snapshot {
        snapshot: Box<ControlState>,
    },
    QueuePage {
        revision: u64,
        total: usize,
        offset: usize,
        entries: Vec<QueueItem>,
    },
    Applied {
        snapshot: Box<ControlState>,
        entry_id: Option<u64>,
    },
    RootAdded {
        root: Root,
    },
    ScanAccepted {
        root_id: u64,
    },
    LibraryRoots {
        #[serde(flatten)]
        page: Page<Root>,
    },
    LibraryArtists {
        #[serde(flatten)]
        page: Page<AlbumArtist>,
    },
    LibraryAlbums {
        #[serde(flatten)]
        page: Page<Album>,
    },
    LibraryTracks {
        #[serde(flatten)]
        page: Page<Track>,
    },
    ListTracks {
        #[serde(flatten)]
        page: Page<ListRow>,
        totals: Option<ListTotals>,
    },
    ListAlbums {
        #[serde(flatten)]
        page: Page<AlbumSummary>,
    },
    ListArtists {
        #[serde(flatten)]
        page: Page<NamedCount>,
        compilations: Option<NamedCount>,
    },
    ListGenres {
        #[serde(flatten)]
        page: Page<NamedCount>,
    },
    AlbumSummaries {
        revision: u64,
        entries: Vec<AlbumSummary>,
    },
    ListGroups {
        revision: u64,
        total: u64,
        groups: Vec<GroupSpan>,
    },
    SavedPlaylists {
        #[serde(flatten)]
        page: Page<Playlist>,
    },
    PlaylistEntries {
        playlist_id: u64,
        library_revision: u64,
        #[serde(flatten)]
        page: Page<PlaylistEntry>,
    },
    PlaylistApplied {
        snapshot: Box<ControlState>,
        playlist: Option<Playlist>,
    },
}

impl From<LibraryReply> for Response {
    fn from(reply: LibraryReply) -> Self {
        match reply {
            LibraryReply::RootAdded(root) => Self::RootAdded { root },
            LibraryReply::ScanAccepted { root_id } => Self::ScanAccepted { root_id },
            LibraryReply::Roots(page) => Self::LibraryRoots { page },
            LibraryReply::Artists(page) => Self::LibraryArtists { page },
            LibraryReply::Albums(page) => Self::LibraryAlbums { page },
            LibraryReply::Tracks(page) => Self::LibraryTracks { page },
            LibraryReply::ListTracks { page, totals } => Self::ListTracks { page, totals },
            LibraryReply::ListAlbums(page) => Self::ListAlbums { page },
            LibraryReply::ListArtists { page, compilations } => {
                Self::ListArtists { page, compilations }
            }
            LibraryReply::ListGenres(page) => Self::ListGenres { page },
            LibraryReply::AlbumSummaries { revision, entries } => {
                Self::AlbumSummaries { revision, entries }
            }
            LibraryReply::ListGroups {
                revision,
                total,
                groups,
            } => Self::ListGroups {
                revision,
                total,
                groups,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    StaleRevision,
    StaleLibraryRevision,
    StalePlaylistRevision,
    InvalidCommand,
    InvalidFilter,
    Playback,
    Persistence,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    Ok {
        result: Response,
    },
    Error {
        code: ErrorCode,
        message: String,
        queue_revision: u64,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        protocol_version: u32,
        instance_id: String,
        snapshot: ControlState,
    },
    State {
        snapshot: ControlState,
    },
    Reply {
        id: u64,
        #[serde(flatten)]
        reply: Reply,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_wire_requires_revision_and_size_and_rejects_client_paths() {
        let command = serde_json::json!({
            "id": 1, "command": "artwork", "album_id": 7, "entry_id": null,
            "library_revision": 2, "size": "thumbnail"
        });
        assert!(matches!(
            serde_json::from_value::<Request>(command.clone())
                .unwrap()
                .command,
            Command::Artwork {
                album_id: Some(7),
                entry_id: None,
                library_revision: 2,
                size: ArtworkSize::Thumbnail
            }
        ));
        for field in ["library_revision", "size"] {
            let mut missing = command.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        for field in ["path", "url"] {
            let mut unsafe_target = command.clone();
            unsafe_target[field] = serde_json::json!("/private/source");
            assert!(serde_json::from_value::<Request>(unsafe_target).is_err());
        }
        let mut invalid = command;
        invalid["size"] = serde_json::json!("original");
        assert!(serde_json::from_value::<Request>(invalid).is_err());
    }

    #[test]
    fn v9_list_commands_require_every_field() {
        assert_eq!(VERSION, 9);
        let list = serde_json::json!({
            "source": {"kind": "playlist", "id": 4, "revision": 9},
            "query": "genre:Jazz",
            "sort": [{"field": "year", "descending": true}],
            "group": "artist_album"
        });
        let commands = [
            serde_json::json!({"id":1,"command":"list_tracks","list":list,
                "revision":null,"offset":0,"limit":10}),
            serde_json::json!({"id":1,"command":"list_albums","list":list,"sort":"date_added",
                "descending":true,"revision":3,"offset":0,"limit":10}),
            serde_json::json!({"id":1,"command":"list_artists","query":"",
                "revision":null,"offset":0,"limit":10}),
            serde_json::json!({"id":1,"command":"list_genres","query":"",
                "revision":null,"offset":0,"limit":10}),
            serde_json::json!({"id":1,"command":"album_summaries","album_ids":[3,1],
                "revision":null}),
            serde_json::json!({"id":1,"command":"list_groups","list":list,"revision":null}),
        ];
        for command in commands {
            let request = serde_json::from_value::<Request>(command.clone())
                .unwrap_or_else(|error| panic!("rejected {command}: {error}"));
            match request.command {
                Command::ListTracks {
                    list,
                    revision: None,
                    offset: 0,
                    limit: 10,
                }
                | Command::ListGroups {
                    list,
                    revision: None,
                } => assert_eq!(list.group, crate::listing::Grouping::ArtistAlbum),
                Command::ListAlbums {
                    sort: AlbumSort::DateAdded,
                    descending: true,
                    revision: Some(3),
                    ..
                } => {}
                Command::ListArtists { query, .. } | Command::ListGenres { query, .. } => {
                    assert!(query.is_empty())
                }
                Command::AlbumSummaries {
                    album_ids,
                    revision: None,
                } => assert_eq!(album_ids, [3, 1]),
                other => panic!("decoded {command} as {other:?}"),
            }
            for field in command.as_object().unwrap().keys() {
                if field == "id" || field == "command" {
                    continue;
                }
                let mut missing = command.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(
                    serde_json::from_value::<Request>(missing).is_err(),
                    "accepted {command} without {field}"
                );
            }
            let mut extra = command.clone();
            extra["path"] = serde_json::json!("/private/source");
            assert!(serde_json::from_value::<Request>(extra).is_err());
        }
        for sort in ["album_artist", "year", "date_added"] {
            let parsed: AlbumSort = serde_json::from_value(serde_json::json!(sort)).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), sort);
        }
    }

    #[test]
    fn list_responses_keep_their_wire_shapes() {
        let response = Response::from(LibraryReply::ListTracks {
            page: Page {
                revision: 2,
                total: 0,
                offset: 0,
                entries: Vec::new(),
            },
            totals: Some(ListTotals {
                tracks: 0,
                duration_ms: 0,
                size_bytes: 0,
            }),
        });
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({"kind":"list_tracks","revision":2,"total":0,"offset":0,"entries":[],
                "totals":{"tracks":0,"duration_ms":0,"size_bytes":0}})
        );
        let groups = Response::from(LibraryReply::ListGroups {
            revision: 2,
            total: 3,
            groups: vec![GroupSpan {
                artist_key: 1,
                album_key: None,
                start: 0,
                count: 3,
            }],
        });
        assert_eq!(
            serde_json::to_value(&groups).unwrap(),
            serde_json::json!({"kind":"list_groups","revision":2,"total":3,
                "groups":[{"artist_key":1,"album_key":null,"start":0,"count":3}]})
        );
    }

    #[test]
    fn enqueue_tracks_wire_shape_round_trips() {
        let list = serde_json::json!({"source":{"kind":"library"},"browser":{"genre_ids":[],
            "artist_ids":[],"compilations":false,"album_ids":[]},"query":"",
            "sort":[],"group":"none"});
        let command = serde_json::json!({"id":1,"command":"enqueue_tracks",
            "selection":{"kind":"list","list":list,"library_revision":3,
                "ranges":[[0,2],[120,u64::MAX]],"excluded":[7],"first_row_id":11},
            "expected_revision":4,"placement":"replace_upcoming","play":true,
            "source":{"kind":"songs","name":"Songs"}});
        let request = serde_json::from_value::<Request>(command.clone()).unwrap();
        let Command::EnqueueTracks {
            selection,
            expected_revision: 4,
            placement: Placement::ReplaceUpcoming,
            play: true,
            source,
        } = &request.command
        else {
            panic!("decoded {:?}", request.command);
        };
        assert_eq!(source, &QueueSource::stamped(SourceKind::Songs, "Songs"));
        let TrackSelection::List {
            ranges,
            excluded,
            first_row_id,
            library_revision: 3,
            ..
        } = selection
        else {
            panic!("decoded {selection:?}");
        };
        assert_eq!(ranges, &[[0, 2], [120, u64::MAX]]);
        assert_eq!((excluded.as_slice(), *first_row_id), (&[7][..], Some(11)));
        assert_eq!(serde_json::to_value(&request).unwrap(), command);
        for field in [
            "selection",
            "expected_revision",
            "placement",
            "play",
            "source",
        ] {
            let mut missing = command.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        for field in ["ranges", "excluded", "first_row_id", "library_revision"] {
            let mut missing = command.clone();
            missing["selection"].as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<Request>(missing).is_err(),
                "accepted a list selection without {field}"
            );
        }
        let ids = serde_json::json!({"kind":"ids","library_revision":3,"track_ids":[5,6]});
        assert_eq!(
            serde_json::from_value::<TrackSelection>(ids.clone()).unwrap(),
            TrackSelection::Ids {
                library_revision: 3,
                track_ids: vec![5, 6]
            }
        );
        let mut extra = ids;
        extra["path"] = serde_json::json!("/private/source");
        assert!(serde_json::from_value::<TrackSelection>(extra).is_err());
        for (placement, name) in [
            (Placement::Append, "append"),
            (Placement::Next, "next"),
            (Placement::ReplaceUpcoming, "replace_upcoming"),
        ] {
            assert_eq!(serde_json::to_value(placement).unwrap(), name);
        }
    }

    #[test]
    fn upcoming_commands_require_every_field() {
        let commands = [
            serde_json::json!({"id":1,"command":"clear_upcoming","expected_revision":3}),
            serde_json::json!({"id":1,"command":"shuffle_upcoming","expected_revision":3}),
            serde_json::json!({"id":1,"command":"save_upcoming","expected_revision":3,
                "expected_playlists_revision":4,"name":"Up Next"}),
        ];
        for command in commands {
            let request = serde_json::from_value::<Request>(command.clone())
                .unwrap_or_else(|error| panic!("rejected {command}: {error}"));
            match &request.command {
                Command::ClearUpcoming {
                    expected_revision: 3,
                }
                | Command::ShuffleUpcoming {
                    expected_revision: 3,
                } => {}
                Command::SaveUpcoming {
                    expected_revision: 3,
                    expected_playlists_revision: 4,
                    name,
                } => assert_eq!(name, "Up Next"),
                other => panic!("decoded {command} as {other:?}"),
            }
            assert_eq!(serde_json::to_value(&request).unwrap(), command);
            for field in command.as_object().unwrap().keys() {
                if field == "id" || field == "command" {
                    continue;
                }
                let mut missing = command.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(serde_json::from_value::<Request>(missing).is_err());
            }
            let mut extra = command.clone();
            extra["path"] = serde_json::json!("/private/source");
            assert!(serde_json::from_value::<Request>(extra).is_err());
        }
    }

    #[test]
    fn playlist_entry_query_is_required_even_when_empty() {
        let mut command = serde_json::json!({
            "id":1,"command":"playlist_entries","playlist_id":1,
            "revision":null,"library_revision":null,"offset":0,"limit":10
        });
        assert!(serde_json::from_value::<Request>(command.clone()).is_err());
        command["query"] = serde_json::json!("");
        let request = serde_json::from_value::<Request>(command).unwrap();
        assert!(
            matches!(request.command, Command::PlaylistEntries { query, .. } if query.is_empty())
        );
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn stamped_names_are_cut_on_a_character_boundary() {
        let short = QueueSource::stamped(SourceKind::Album, "Kind of Blue");
        assert_eq!(short.name, "Kind of Blue");
        // 255 ASCII bytes then a 2-byte character straddles the 256-byte limit.
        let name = format!("{}é", "a".repeat(255));
        let cut = QueueSource::stamped(SourceKind::Playlist, &name);
        assert_eq!(cut.name, "a".repeat(255));
        let exact = QueueSource::stamped(SourceKind::Playlist, &"é".repeat(128));
        assert_eq!(exact.name.len(), MAX_SOURCE_NAME_BYTES);
    }
    #[test]
    fn repeat_scope_follows_source_kind() {
        assert_eq!(SourceKind::Smart.repeat_scope(), RepeatScope::Run);
        assert_eq!(SourceKind::Drag.repeat_scope(), RepeatScope::Queue);
        assert_eq!(
            SourceKind::parse("compilations"),
            Some(SourceKind::Compilations)
        );
        assert_eq!(
            serde_json::to_string(&SourceKind::Compilations).unwrap(),
            "\"compilations\""
        );
    }
}
