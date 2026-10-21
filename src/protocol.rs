//! Version 18 local control protocol. Queue/library/playlist pages are revision-bound; reconnect
//! replaces cached control state. A missing reply never authorizes command replay.
use crate::library_types::{
    Album, AlbumArtist, AlbumSort, AlbumSummary, GroupSpan, LibraryReply, LibraryState, ListRow,
    ListTotals, NamedCount, Page, Root, Track, TrackMetadata,
};
use crate::listing::ListDescription;
use crate::stats_types::{PeriodRequest, StatsReport, WrappedReport};
use crate::{
    decoder::GainMode,
    equalizer::EqualizerSettings,
    play_order::{RepeatMode, RepeatScope},
};
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;

pub const VERSION: u32 = 18;
pub const MAX_PAGE_SIZE: usize = 500;
/// The most tracks one enqueue or playlist add may resolve to.
pub const MAX_RUN_ENTRIES: usize = 100_000;
/// The most saved radio stations the catalog may hold.
pub const MAX_STATIONS: usize = 1000;
/// The most stations one `add_stations` may carry. The Swift client's
/// `StationCatalog.maximumBatch` must match (`tests/swift_constants.rs`).
pub const MAX_STATION_BATCH: usize = 500;
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
    Lyrics {
        entry_id: u64,
        library_revision: u64,
    },
    Stats {
        period: PeriodRequest,
    },
    Wrapped {
        year: i32,
    },
    StatsYears,
    /// Looks an album up on MusicBrainz into a stored proposal. Nothing is written.
    MusicbrainzLookup {
        album_id: u64,
        library_revision: u64,
        release_id: Option<String>,
        #[serde(default)]
        fingerprint: bool,
        acoustid_key: Option<String>,
    },
    /// Writes a stored proposal's values to the named tracks. The client sends
    /// no tag values.
    ApplyTags {
        proposal_id: String,
        track_ids: Vec<u64>,
        #[serde(default)]
        omit_fields: Vec<TagField>,
    },
    TagEdits,
    RevertTagEdit {
        edit_id: u64,
    },
    SearchDirectory {
        query: String,
    },
    CountDirectoryClick {
        station_uuid: String,
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
    AppendStream {
        expected_revision: u64,
        url: String,
        title: String,
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
    /// Removes the upcoming entries; the current one keeps playing. Upcoming
    /// means: with shuffle off, the entries after the current one in queue
    /// order; with shuffle on, every unplayed entry other than the current one;
    /// with nothing current, every unplayed entry. `ControlState::upcoming_count`
    /// counts them.
    ClearUpcoming {
        expected_revision: u64,
    },
    /// Puts the upcoming entries (as `ClearUpcoming` defines them) in a uniform
    /// random order. Other entries keep their places.
    ShuffleUpcoming {
        expected_revision: u64,
    },
    /// Creates a regular playlist of the upcoming entries (as `ClearUpcoming`
    /// defines them) that have a library track, in queue order.
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
    SetEqualizer {
        equalizer: EqualizerSettings,
    },
    SaveEqualizerPreset {
        name: String,
        equalizer: EqualizerSettings,
    },
    RenameEqualizerPreset {
        name: String,
        new_name: String,
    },
    RemoveEqualizerPreset {
        name: String,
    },
    SetResume {
        enabled: bool,
    },
    SetRepeat {
        mode: RepeatMode,
    },
    SetShuffle {
        enabled: bool,
    },
    SetVolume {
        percent: u8,
    },
    AddRoot {
        path: String,
    },
    Rescan {
        root_id: u64,
    },
    SetTrackLiked {
        track_id: u64,
        liked: bool,
        library_revision: u64,
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
        /// Restricts artists to those with rows in this list; `null` for none.
        #[serde(deserialize_with = "required")]
        list: Option<ListDescription>,
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
    /// Counted plays, newest first. A `revision` other than the current
    /// history revision is not rejected: the reply carries the current one.
    History {
        #[serde(deserialize_with = "required")]
        revision: Option<u64>,
        offset: usize,
        limit: usize,
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
    Stations,
    AddStations {
        expected_revision: u64,
        stations: Vec<StationInput>,
    },
    UpdateStation {
        station_id: u64,
        expected_revision: u64,
        name: String,
        url: String,
    },
    RemoveStation {
        station_id: u64,
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
    /// After removing the upcoming entries (as `Command::ClearUpcoming`
    /// defines them), after the last kept entry.
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricsStatus {
    Ready,
    Missing,
    Unavailable,
    Instrumental,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSource {
    Embedded,
    Lrclib,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Lyrics {
    pub status: LyricsStatus,
    pub source: Option<LyricsSource>,
    pub plain_text: Option<String>,
    pub synced_lrc: Option<String>,
    pub attribution_url: Option<String>,
    pub message: Option<String>,
}

/// One journaled tag edit, as listed by `tag_edits`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TagEditSummary {
    pub edit_id: u64,
    pub created_at: i64,
    pub release_title: String,
    pub file_count: u32,
    pub reverted: bool,
}

/// A tag field that a MusicBrainz lookup can propose and Tyranny can write.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TagField {
    Title,
    Artist,
    Album,
    AlbumArtist,
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
    Date,
    OriginalDate,
    MusicbrainzRecordingId,
    MusicbrainzTrackId,
    MusicbrainzReleaseId,
    MusicbrainzReleaseGroupId,
    MusicbrainzArtistId,
    MusicbrainzAlbumArtistId,
}

impl TagField {
    pub const ALL: [TagField; 16] = [
        TagField::Title,
        TagField::Artist,
        TagField::Album,
        TagField::AlbumArtist,
        TagField::TrackNumber,
        TagField::TrackTotal,
        TagField::DiscNumber,
        TagField::DiscTotal,
        TagField::Date,
        TagField::OriginalDate,
        TagField::MusicbrainzRecordingId,
        TagField::MusicbrainzTrackId,
        TagField::MusicbrainzReleaseId,
        TagField::MusicbrainzReleaseGroupId,
        TagField::MusicbrainzArtistId,
        TagField::MusicbrainzAlbumArtistId,
    ];

    /// Track and disc numbers and totals, which compare as numbers.
    pub fn is_number(self) -> bool {
        matches!(
            self,
            TagField::TrackNumber
                | TagField::TrackTotal
                | TagField::DiscNumber
                | TagField::DiscTotal
        )
    }
}

/// One field of one file that a lookup proposes to change.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FieldChange {
    pub field: TagField,
    pub old: Option<String>,
    pub new: String,
}

/// A release a lookup considered, with its score.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TagCandidate {
    pub release_id: String,
    pub title: String,
    pub artist: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub track_count: u32,
    pub score: u32,
}

/// What a lookup proposes for one file of the album.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrackProposal {
    pub track_id: u64,
    pub file_name: String,
    /// The matched release position, else the stored track number.
    pub track_number: Option<u32>,
    pub matched: bool,
    pub writable: bool,
    pub reason: Option<String>,
    pub removes_other_copies: bool,
    pub unwritable_fields: Vec<TagField>,
    pub changes: Vec<FieldChange>,
}

/// The reply to `musicbrainz_lookup`. `candidates` is sorted by score, best first.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TagProposal {
    pub proposal_id: String,
    pub release_id: String,
    pub candidates: Vec<TagCandidate>,
    pub tracks: Vec<TrackProposal>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TagFileOutcome {
    Written,
    Skipped,
    Failed,
}

/// What `apply_tags` or `revert_tag_edit` did to one file. `message` says why
/// a file was skipped or failed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TagFileResult {
    pub track_id: u64,
    pub file_name: String,
    pub outcome: TagFileOutcome,
    pub message: Option<String>,
}

/// The reply to `apply_tags` and `revert_tag_edit`. `edit_id` is `None` when
/// no file reached the journal.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TagApplyResult {
    pub edit_id: Option<u64>,
    pub files: Vec<TagFileResult>,
}

/// A server-owned named parametric EQ configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EqualizerUserPreset {
    pub name: String,
    pub equalizer: EqualizerSettings,
}

pub fn validate_equalizer_preset_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.trim() == name
            && name.chars().count() <= 80
            && !name.chars().any(char::is_control),
        "preset name must be trimmed, nonempty, at most 80 characters, and contain no controls"
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Settings {
    pub gain_mode: GainMode,
    pub resume_on_start: bool,
    #[serde(default)]
    pub repeat: RepeatMode,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default = "full")]
    pub volume: u8,
    #[serde(default)]
    pub equalizer: EqualizerSettings,
    #[serde(default)]
    pub equalizer_presets: Vec<EqualizerUserPreset>,
}

fn full() -> u8 {
    100
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            gain_mode: GainMode::Off,
            resume_on_start: false,
            repeat: RepeatMode::Off,
            shuffle: false,
            volume: full(),
            equalizer: EqualizerSettings::default(),
            equalizer_presets: Vec::new(),
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
    /// `album_id`/`album_artist_id`/`compilation`/`format` from the library; the
    /// player leaves them blank.
    pub track_id: Option<u64>,
    pub album_id: Option<u64>,
    /// The album's artist, as in `TrackSummary`, not the track artist.
    pub album_artist_id: Option<u64>,
    /// Whether the album is a compilation.
    pub compilation: Option<bool>,
    pub format: String,
    /// Gain the decoder applied; `None` when gain mode is off.
    pub gain_db: Option<f64>,
    /// The saved station name of a live stream; `None` for a library entry.
    #[serde(default)]
    pub station: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ControlState {
    pub sequence: u64,
    pub queue_revision: u64,
    pub playlists_revision: u64,
    pub stations_revision: u64,
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
    /// Changes when a play counts: the history and the play counts change then.
    pub history_revision: u64,
    /// Changes on every play history write: a play opening, counting,
    /// saving its listened time or closing.
    pub stats_revision: u64,
    /// The queue position of `current_id`; `None` with nothing current.
    pub current_index: Option<usize>,
    /// The upcoming entries, as `Command::ClearUpcoming` defines them.
    pub upcoming_count: usize,
}

/// A live HTTP audio source; never interpreted as a filesystem path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamSpec {
    pub url: String,
    pub title: String,
}
impl StreamSpec {
    pub fn new(url: String, title: String) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !url.is_empty() && url.len() <= 8192 && !url.chars().any(char::is_control),
            "stream URL must contain 1..=8192 bytes and no control characters"
        );
        anyhow::ensure!(
            title.len() <= MAX_SOURCE_NAME_BYTES && !title.chars().any(char::is_control),
            "stream title must contain at most {MAX_SOURCE_NAME_BYTES} bytes and no control characters"
        );
        let parsed = reqwest::Url::parse(&url)?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some_and(|host| !host.is_empty())
                && parsed.username().is_empty()
                && parsed.password().is_none(),
            "stream URL requires HTTP(S), a host, and no credentials"
        );
        let title = if title.trim().is_empty() {
            parsed.host_str().expect("validated host").to_owned()
        } else {
            title.trim().to_owned()
        };
        let url = parsed.to_string();
        anyhow::ensure!(url.len() <= 8192, "canonical stream URL is too long");
        anyhow::ensure!(
            title.len() <= MAX_SOURCE_NAME_BYTES,
            "stream title is too long"
        );
        Ok(Self { url, title })
    }
}

/// A saved radio station: a canonical stream URL and its display name.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Station {
    pub id: u64,
    pub name: String,
    pub url: String,
}

/// One radio-browser.info search hit, already filtered to something playable.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DirectoryStation {
    pub uuid: String,
    pub name: String,
    pub url: String,
    pub codec: String,
    pub bitrate: u32,
    pub country_code: String,
    pub tags: Vec<String>,
}

/// A station as a client submits it. A blank name becomes the URL's host.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StationInput {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct QueueItem {
    pub id: u64,
    pub path: String,
    pub stream: Option<StreamSpec>,
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

/// One counted play, as the History view shows it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: u64,
    /// UTC seconds.
    pub played_at: i64,
    /// `None` for an unindexed (direct-path) entry.
    pub track_id: Option<u64>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub source: Option<QueueSource>,
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
    Stream,
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
            Self::Stream => "stream",
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
            Self::Stream,
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
    Lyrics {
        lyrics: Lyrics,
    },
    Stats {
        stats_revision: u64,
        report: Box<StatsReport>,
    },
    Wrapped {
        stats_revision: u64,
        report: Box<WrappedReport>,
    },
    StatsYears {
        years: Vec<i32>,
    },
    TagProposal {
        proposal: TagProposal,
    },
    TagApply {
        #[serde(flatten)]
        result: TagApplyResult,
    },
    TagEdits {
        edits: Vec<TagEditSummary>,
    },
    DirectoryResults {
        stations: Vec<DirectoryStation>,
    },
    DirectoryClickCounted,
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
    TrackLiked {
        track_id: u64,
        liked: bool,
        library_revision: u64,
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
    Stations {
        revision: u64,
        stations: Vec<Station>,
    },
    StationsApplied {
        snapshot: Box<ControlState>,
        stations: Vec<Station>,
    },
    History {
        #[serde(flatten)]
        page: Page<HistoryEntry>,
    },
}

impl From<LibraryReply> for Response {
    fn from(reply: LibraryReply) -> Self {
        match reply {
            LibraryReply::RootAdded(root) => Self::RootAdded { root },
            LibraryReply::ScanAccepted { root_id } => Self::ScanAccepted { root_id },
            LibraryReply::TrackLiked {
                track_id,
                liked,
                library_revision,
            } => Self::TrackLiked {
                track_id,
                liked,
                library_revision,
            },
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
            LibraryReply::ReindexQueued { .. } => {
                unreachable!("Reindex is internal to tagging and never reaches the wire")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    StaleRevision,
    StaleLibraryRevision,
    StalePlaylistRevision,
    StaleStationsRevision,
    InvalidCommand,
    InvalidFilter,
    Playback,
    Persistence,
    Lookup,
    Unavailable,
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
    fn equalizer_wire_requires_a_complete_parametric_configuration() {
        let equalizer = EqualizerSettings::from_graphic(true, -6.0, [3.0; 10]);
        let value = serde_json::to_value(Request {
            id: 1,
            command: Command::SetEqualizer {
                equalizer: equalizer.clone(),
            },
        })
        .unwrap();
        assert_eq!(value["command"], "set_equalizer");
        assert_eq!(value["equalizer"]["preamp_db"], -6.0);
        match serde_json::from_value::<Request>(value.clone())
            .unwrap()
            .command
        {
            Command::SetEqualizer { equalizer: decoded } => assert_eq!(decoded, equalizer),
            command => panic!("unexpected command: {command:?}"),
        }
        for field in ["enabled", "preamp_db", "bands"] {
            let mut missing = value.clone();
            missing["equalizer"].as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        let mut legacy = value.clone();
        legacy["equalizer"]["bands_db"] = serde_json::json!(vec![0.0; 10]);
        assert!(serde_json::from_value::<Request>(legacy).is_err());
        for field in [
            "enabled",
            "kind",
            "frequency_hz",
            "gain_db",
            "q",
            "shelf_slope",
            "slope_db",
        ] {
            let mut missing = value.clone();
            missing["equalizer"]["bands"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("equalizer");
        assert!(serde_json::from_value::<Request>(missing).is_err());
        let settings = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(
            settings["equalizer"],
            serde_json::json!({
                "enabled": false, "preamp_db": 0.0, "bands": [],
            })
        );
    }

    #[test]
    fn stream_specs_validate_without_network_and_canonicalize() {
        let spec = StreamSpec::new("HTTPS://EXAMPLE.ORG:443/live".into(), "  ".into()).unwrap();
        assert_eq!(spec.url, "https://example.org/live");
        assert_eq!(spec.title, "example.org");
        for url in [
            "file:///music/song.mp3",
            "ftp://example.org/live",
            "https://",
            "http://user:secret@example.org/live",
            "http://user@example.org/live",
            "http://example.org/\ntrack",
            "",
        ] {
            assert!(
                StreamSpec::new(url.into(), "Station".into()).is_err(),
                "{url:?}"
            );
        }
        assert!(StreamSpec::new("http://example.org".into(), "x".repeat(257)).is_err());
        assert!(StreamSpec::new("http://example.org".into(), "bad\ntitle".into()).is_err());
        assert!(
            StreamSpec::new(
                format!("http://example.org/{}", "x".repeat(8192)),
                "".into()
            )
            .is_err()
        );
        let request: Request = serde_json::from_value(serde_json::json!({
            "id": 1, "command": "append_stream", "expected_revision": 2,
            "url": spec.url, "title": spec.title
        }))
        .unwrap();
        assert!(matches!(
            request.command,
            Command::AppendStream {
                expected_revision: 2,
                ..
            }
        ));
        assert_eq!(SourceKind::parse("stream"), Some(SourceKind::Stream));
    }

    #[test]
    fn liked_mutation_requires_boolean_state_and_revision_and_replies_with_revision() {
        let command = serde_json::json!({
            "id": 1, "command": "set_track_liked", "track_id": 7,
            "liked": true, "library_revision": 2
        });
        assert!(matches!(
            serde_json::from_value::<Request>(command.clone())
                .unwrap()
                .command,
            Command::SetTrackLiked {
                track_id: 7,
                liked: true,
                library_revision: 2
            }
        ));
        for field in ["track_id", "liked", "library_revision"] {
            let mut missing = command.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        for value in [
            serde_json::json!("true"),
            serde_json::json!(1),
            serde_json::Value::Null,
        ] {
            let mut invalid = command.clone();
            invalid["liked"] = value;
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
        let response = Response::from(LibraryReply::TrackLiked {
            track_id: 7,
            liked: true,
            library_revision: 3,
        });
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "kind": "track_liked", "track_id": 7, "liked": true, "library_revision": 3
            })
        );
    }

    #[test]
    fn station_commands_use_snake_case_wire_names() {
        let decode = |value: serde_json::Value| serde_json::from_value::<Request>(value).unwrap();
        assert!(matches!(
            decode(serde_json::json!({"id":1,"command":"stations"})).command,
            Command::Stations
        ));
        assert!(matches!(
            decode(serde_json::json!({"id":1,"command":"add_stations","expected_revision":0,
                "stations":[{"name":"","url":"https://a.example/"}]}))
            .command,
            Command::AddStations { expected_revision: 0, stations } if stations.len() == 1
        ));
        assert!(matches!(
            decode(
                serde_json::json!({"id":1,"command":"update_station","station_id":3,
                "expected_revision":1,"name":"A","url":"https://a.example/"})
            )
            .command,
            Command::UpdateStation { station_id: 3, .. }
        ));
        assert!(matches!(
            decode(
                serde_json::json!({"id":1,"command":"remove_station","station_id":3,
                "expected_revision":1})
            )
            .command,
            Command::RemoveStation { station_id: 3, .. }
        ));
    }

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
    fn lyrics_wire_requires_identity_and_rejects_paths() {
        let command = serde_json::json!({
            "id": 1, "command": "lyrics", "entry_id": 7, "library_revision": 2
        });
        assert!(matches!(
            serde_json::from_value::<Request>(command.clone())
                .unwrap()
                .command,
            Command::Lyrics {
                entry_id: 7,
                library_revision: 2
            }
        ));
        for field in ["entry_id", "library_revision"] {
            let mut missing = command.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        for field in ["path", "url", "title", "artist"] {
            let mut unsafe_target = command.clone();
            unsafe_target[field] = serde_json::json!("untrusted");
            assert!(serde_json::from_value::<Request>(unsafe_target).is_err());
        }
    }

    #[test]
    fn tag_commands_use_snake_case_wire_names_and_refuse_client_values() {
        let decode = |value: serde_json::Value| serde_json::from_value::<Request>(value);
        let lookup = serde_json::json!({"id":1,"command":"musicbrainz_lookup",
            "album_id":7,"library_revision":2});
        assert!(matches!(
            decode(lookup.clone()).unwrap().command,
            Command::MusicbrainzLookup {
                album_id: 7,
                library_revision: 2,
                release_id: None,
                fingerprint: false,
                acoustid_key: None
            }
        ));
        let full = serde_json::json!({"id":1,"command":"musicbrainz_lookup","album_id":7,
            "library_revision":2,"release_id":"r","fingerprint":true,"acoustid_key":"k"});
        match decode(full).unwrap().command {
            Command::MusicbrainzLookup {
                release_id: Some(release_id),
                fingerprint: true,
                acoustid_key: Some(key),
                ..
            } => assert_eq!((release_id.as_str(), key.as_str()), ("r", "k")),
            other => panic!("decoded as {other:?}"),
        }
        let apply = serde_json::json!({"id":1,"command":"apply_tags","proposal_id":"p",
            "track_ids":[3,4]});
        match decode(apply.clone()).unwrap().command {
            Command::ApplyTags {
                proposal_id,
                track_ids,
                omit_fields,
            } => {
                assert_eq!((proposal_id.as_str(), track_ids), ("p", vec![3, 4]));
                assert!(omit_fields.is_empty());
            }
            other => panic!("decoded as {other:?}"),
        }
        let mut omitting = apply.clone();
        omitting["omit_fields"] = serde_json::json!(["album_artist", "date"]);
        assert!(matches!(
            decode(omitting).unwrap().command,
            Command::ApplyTags { omit_fields, .. }
                if omit_fields == [TagField::AlbumArtist, TagField::Date]
        ));
        assert!(matches!(
            decode(serde_json::json!({"id":1,"command":"tag_edits"}))
                .unwrap()
                .command,
            Command::TagEdits
        ));
        let revert = serde_json::json!({"id":1,"command":"revert_tag_edit","edit_id":9});
        assert!(matches!(
            decode(revert.clone()).unwrap().command,
            Command::RevertTagEdit { edit_id: 9 }
        ));

        let mut with_path = lookup.clone();
        with_path["path"] = serde_json::json!("/private/source");
        assert!(decode(with_path).is_err());
        for field in ["values", "tags", "path"] {
            let mut untrusted = apply.clone();
            untrusted[field] = serde_json::json!({"title":"untrusted"});
            assert!(decode(untrusted).is_err(), "{field} was accepted");
        }
        for (command, field) in [
            (&lookup, "album_id"),
            (&lookup, "library_revision"),
            (&apply, "proposal_id"),
            (&apply, "track_ids"),
            (&revert, "edit_id"),
        ] {
            let mut missing = command.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(decode(missing).is_err(), "missing {field} was accepted");
        }

        let proposal = TagProposal {
            proposal_id: "p".into(),
            release_id: "r".into(),
            candidates: Vec::new(),
            tracks: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(Response::TagProposal { proposal }).unwrap(),
            serde_json::json!({"kind":"tag_proposal","proposal":{"proposal_id":"p",
                "release_id":"r","candidates":[],"tracks":[]}})
        );
        let result = TagApplyResult {
            edit_id: Some(3),
            files: vec![TagFileResult {
                track_id: 4,
                file_name: "01 a.flac".into(),
                outcome: TagFileOutcome::Written,
                message: None,
            }],
        };
        assert_eq!(
            serde_json::to_value(Response::TagApply { result }).unwrap(),
            serde_json::json!({"kind":"tag_apply","edit_id":3,"files":[{"track_id":4,
                "file_name":"01 a.flac","outcome":"written","message":null}]})
        );
        let edits = vec![TagEditSummary {
            edit_id: 3,
            created_at: 1_792_575_000,
            release_title: "Fixture Album".into(),
            file_count: 1,
            reverted: false,
        }];
        assert_eq!(
            serde_json::to_value(Response::TagEdits { edits }).unwrap(),
            serde_json::json!({"kind":"tag_edits","edits":[{"edit_id":3,
                "created_at":1_792_575_000,"release_title":"Fixture Album","file_count":1,
                "reverted":false}]})
        );
        assert_eq!(
            serde_json::to_value(ErrorCode::Lookup).unwrap(),
            serde_json::json!("lookup")
        );
    }

    #[test]
    fn directory_commands_and_responses_round_trip() {
        let search = serde_json::json!({"id":1,"command":"search_directory","query":"jazz"});
        match serde_json::from_value::<Request>(search.clone())
            .unwrap()
            .command
        {
            Command::SearchDirectory { query } => assert_eq!(query, "jazz"),
            other => panic!("decoded as {other:?}"),
        }
        let mut extra = search;
        extra["limit"] = serde_json::json!(5);
        assert!(serde_json::from_value::<Request>(extra).is_err());
        let uuid = "9617a958-0601-11e8-ae97-52543be04c81";
        let click =
            serde_json::json!({"id":2,"command":"count_directory_click","station_uuid":uuid});
        match serde_json::from_value::<Request>(click).unwrap().command {
            Command::CountDirectoryClick { station_uuid } => assert_eq!(station_uuid, uuid),
            other => panic!("decoded as {other:?}"),
        }
        let results = Response::DirectoryResults {
            stations: vec![DirectoryStation {
                uuid: uuid.into(),
                name: "Jazz FM".into(),
                url: "https://radio.example/jazz.mp3".into(),
                codec: "MP3".into(),
                bitrate: 128,
                country_code: "GB".into(),
                tags: vec!["jazz".into()],
            }],
        };
        let json = serde_json::to_value(&results).unwrap();
        assert_eq!(json["kind"], "directory_results");
        assert_eq!(json["stations"][0]["uuid"], uuid);
        let counted = serde_json::to_value(Response::DirectoryClickCounted).unwrap();
        assert_eq!(
            counted,
            serde_json::json!({"kind":"directory_click_counted"})
        );
        let back: Response = serde_json::from_value(json).unwrap();
        assert!(matches!(back, Response::DirectoryResults { stations } if stations.len() == 1));
        assert!(matches!(
            serde_json::from_value::<Response>(counted).unwrap(),
            Response::DirectoryClickCounted
        ));
    }

    #[test]
    fn unavailable_error_code_is_snake_case() {
        assert_eq!(
            serde_json::to_value(ErrorCode::Unavailable).unwrap(),
            "unavailable"
        );
    }

    #[test]
    fn list_artists_list_field_is_required_and_nullable() {
        let base = |list: Option<serde_json::Value>| {
            let mut request = serde_json::json!({"id":1,"command":"list_artists","query":"",
                "revision":null,"offset":0,"limit":10});
            if let Some(list) = list {
                request["list"] = list;
            }
            serde_json::from_value::<Request>(request)
        };
        assert!(base(None).is_err());
        match base(Some(serde_json::Value::Null)).unwrap().command {
            Command::ListArtists { list: None, .. } => {}
            other => panic!("decoded as {other:?}"),
        }
        let list = serde_json::json!({"source":{"kind":"library"},
            "browser":{"genre_ids":[3]},"query":"","sort":[],"group":"none"});
        match base(Some(list)).unwrap().command {
            Command::ListArtists {
                list: Some(list), ..
            } => assert_eq!(list.browser.genre_ids, [3]),
            other => panic!("decoded as {other:?}"),
        }
    }

    #[test]
    fn v9_list_commands_require_every_field() {
        assert_eq!(VERSION, 18);
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
            serde_json::json!({"id":1,"command":"list_artists","query":"","list":null,
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

    #[test]
    fn history_requires_every_field_and_replies_as_a_page() {
        let command = serde_json::json!({
            "id":1,"command":"history","revision":null,"offset":0,"limit":10
        });
        let request = serde_json::from_value::<Request>(command.clone()).unwrap();
        assert!(matches!(
            request.command,
            Command::History {
                revision: None,
                offset: 0,
                limit: 10
            }
        ));
        for field in ["revision", "offset", "limit"] {
            let mut missing = command.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<Request>(missing).is_err(),
                "{field}"
            );
        }
        let response = Response::History {
            page: Page {
                revision: 4,
                total: 1,
                offset: 0,
                entries: vec![HistoryEntry {
                    id: 4,
                    played_at: 1_700_000_000,
                    track_id: None,
                    title: "Loose".into(),
                    artist: String::new(),
                    album: String::new(),
                    duration_ms: None,
                    source: None,
                }],
            },
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({"kind":"history","revision":4,"total":1,"offset":0,
                "entries":[{"id":4,"played_at":1_700_000_000_i64,"track_id":null,"title":"Loose",
                    "artist":"","album":"","duration_ms":null,"source":null}]})
        );
    }

    #[test]
    fn stats_commands_require_every_field_and_reply_with_a_report() {
        let command = serde_json::json!({
            "id":1,"command":"stats","period":{"kind":"week","anchor":"2026-10-21"}
        });
        let request = serde_json::from_value::<Request>(command.clone()).unwrap();
        match request.command {
            Command::Stats { period } => {
                assert_eq!(period.kind, crate::stats_types::PeriodKind::Week);
                assert_eq!(period.anchor, "2026-10-21");
            }
            other => panic!("not stats: {other:?}"),
        }
        let mut missing = command.clone();
        missing.as_object_mut().unwrap().remove("period");
        assert!(serde_json::from_value::<Request>(missing).is_err());
        for bad in [
            serde_json::json!({"kind":"week"}),
            serde_json::json!({"anchor":"2026-10-21"}),
            serde_json::json!({"kind":"Week","anchor":"2026-10-21"}),
            serde_json::json!({"kind":"week","anchor":"2026-10-21","zone":"UTC"}),
        ] {
            let mut invalid = command.clone();
            invalid["period"] = bad;
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
        let wrapped = serde_json::json!({"id":2,"command":"wrapped","year":2025});
        assert!(matches!(
            serde_json::from_value::<Request>(wrapped.clone())
                .unwrap()
                .command,
            Command::Wrapped { year: 2025 }
        ));
        let mut missing = wrapped;
        missing.as_object_mut().unwrap().remove("year");
        assert!(serde_json::from_value::<Request>(missing).is_err());
        assert!(matches!(
            serde_json::from_value::<Request>(serde_json::json!({"id":3,"command":"stats_years"}))
                .unwrap()
                .command,
            Command::StatsYears
        ));
        assert_eq!(
            serde_json::to_value(Response::StatsYears {
                years: vec![2025, 2026]
            })
            .unwrap(),
            serde_json::json!({"kind":"stats_years","years":[2025,2026]})
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
