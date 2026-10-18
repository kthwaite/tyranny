//! Version 8 local control protocol. Queue/library/playlist pages are revision-bound; reconnect
//! replaces cached control state. A missing reply never authorizes command replay.
use crate::decoder::GainMode;
use crate::library_types::{
    Album, AlbumArtist, LibraryReply, LibraryState, Page, Root, Track, TrackMetadata,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const VERSION: u32 = 8;
pub const MAX_PAGE_SIZE: usize = 500;

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NowPlaying {
    pub entry_id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_frames: Option<u64>,
    pub metadata_error: Option<String>,
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
