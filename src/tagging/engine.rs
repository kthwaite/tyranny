//! The worker's state, and the lookup that turns an album into a stored proposal.
use super::{
    Failure, LOOKUP_DEADLINE, LookupError, LookupRequest, MAX_ALBUM_TRACKS,
    acoustid::{AcoustId, rank_releases},
    fingerprint::fingerprint,
    journal::Journal,
    matching::{
        AlbumFacts, AlbumTrack, Evidence, TrackRef, changes, match_files, most_common_release,
        proposed, score,
    },
    musicbrainz::{MusicBrainz, Release, SearchQuery, is_mbid},
    proposal::{Proposal, Proposals, ProposedFile, ReleaseCache},
    safe_write::stamp,
    tags::{read_tags, unwritable_fields_in, writable_format},
};
use crate::cancel::Cancellation;
use crate::library_types::TrackMetadata;
use crate::protocol::{ErrorCode, TagCandidate, TagField, TagProposal, TrackProposal};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CANT_WRITE_FORMAT: &str = "Tyranny can't write tags to this format";
const NO_MATCHING_TRACK: &str = "No matching track on this release";
const NO_RELEASE: &str = "MusicBrainz has no matching release";
const NO_FINGERPRINT_MATCH: &str = "AcoustID has no match for these files";

pub(super) struct Providers {
    pub musicbrainz: Result<MusicBrainz, String>,
    pub acoustid: Result<AcoustId, String>,
}
impl Providers {
    pub(super) fn live() -> Self {
        Self {
            musicbrainz: MusicBrainz::new().map_err(|e| format!("{e:#}")),
            acoustid: AcoustId::new().map_err(|e| format!("{e:#}")),
        }
    }
}

pub(super) struct Engine {
    database: Connection,
    journal_path: PathBuf,
    journal: Option<Journal>,
    providers: Providers,
    proposals: Proposals,
    releases: ReleaseCache,
}

/// Why a lookup stopped, before `queue_revision` is known.
struct Refusal {
    code: ErrorCode,
    message: String,
}
impl Refusal {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidCommand, message)
    }
    fn lookup(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Lookup, message)
    }
}
impl From<LookupError> for Refusal {
    fn from(error: LookupError) -> Self {
        Self::lookup(error.to_string())
    }
}
impl From<rusqlite::Error> for Refusal {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(
            ErrorCode::Persistence,
            format!("reading the library: {error}"),
        )
    }
}

impl Engine {
    pub(super) fn open(database: &Path, providers: Providers) -> anyhow::Result<Self> {
        let connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        let journal_path = database.with_extension("tagging.sqlite");
        Ok(Self {
            database: connection,
            journal: Journal::open(&journal_path).ok(),
            journal_path,
            providers,
            proposals: Proposals::default(),
            releases: ReleaseCache::default(),
        })
    }

    pub(super) fn lookup(
        &mut self,
        request: &LookupRequest,
        cancel: &Cancellation,
    ) -> Result<TagProposal, Failure> {
        let deadline = Instant::now() + LOOKUP_DEADLINE;
        self.find(request, cancel, deadline)
            .map_err(|refusal| self.failure(refusal))
    }

    fn failure(&self, refusal: Refusal) -> Failure {
        let queue_revision = self
            .database
            .query_row("SELECT revision FROM control WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        Failure {
            code: refusal.code,
            message: refusal.message,
            queue_revision,
        }
    }

    fn find(
        &mut self,
        request: &LookupRequest,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<TagProposal, Refusal> {
        validate(request)?;
        let (album, tracks) = self.album(request)?;
        let candidates = self.candidates(request, &album, &tracks, cancel, deadline)?;
        let musicbrainz = self
            .providers
            .musicbrainz
            .as_ref()
            .map_err(Refusal::lookup)?;
        let mut scored: Vec<(u32, Release)> = Vec::with_capacity(candidates.len());
        for (id, evidence) in candidates {
            let release = fetch(musicbrainz, &mut self.releases, &id, cancel, deadline)?;
            scored.push((score(&album, &release, evidence), release));
        }
        // The best score wins; on a tie, the earlier candidate. An explicit
        // release is the only candidate.
        let mut chosen = 0;
        for (index, (points, _)) in scored.iter().enumerate() {
            if *points > scored[chosen].0 {
                chosen = index;
            }
        }
        let release = scored[chosen].1.clone();
        scored.sort_by_key(|(points, _)| std::cmp::Reverse(*points));

        let matches = match_files(&tracks, &release);
        let mut rows = Vec::with_capacity(tracks.len());
        let mut files = Vec::new();
        for (track, at) in tracks.iter().zip(matches) {
            cancel.check_lookup(deadline)?;
            let (row, file) = propose_file(track, &release, at);
            rows.push(row);
            files.extend(file);
        }
        let proposal_id = self.proposals.insert(
            Instant::now(),
            Proposal {
                release_id: release.id.clone(),
                release_title: release.title.clone(),
                files,
            },
        );
        Ok(TagProposal {
            proposal_id,
            release_id: release.id.clone(),
            candidates: scored
                .iter()
                .map(|(points, release)| TagCandidate {
                    release_id: release.id.clone(),
                    title: release.title.clone(),
                    artist: release.artist(),
                    date: release.date.clone(),
                    country: release.country.clone(),
                    track_count: release.track_count(),
                    score: *points,
                })
                .collect(),
            tracks: rows,
        })
    }

    /// The album's facts and its indexed tracks, in disc and track order, read
    /// in one transaction.
    fn album(&mut self, request: &LookupRequest) -> Result<(AlbumFacts, Vec<AlbumTrack>), Refusal> {
        let tx = self.database.transaction()?;
        let revision: u64 = tx.query_row(
            "SELECT revision FROM library_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if revision != request.library_revision {
            return Err(Refusal::new(
                ErrorCode::StaleLibraryRevision,
                format!("library revision is {revision}"),
            ));
        }
        let unknown = || Refusal::invalid("unknown album");
        let id = i64::try_from(request.album_id).map_err(|_| unknown())?;
        let (title, artist): (String, String) = tx
            .query_row(
                "SELECT title,album_artist FROM library_albums WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(unknown)?;
        let mut tracks = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT t.id,a.path,t.metadata FROM library_tracks t
                 JOIN library_assets a ON a.id=t.asset_id
                 JOIN library_roots r ON r.id=a.root_id
                 WHERE t.album_id=?1 AND a.indexed=1 AND a.last_seen_scan>=r.completed_scan
                 ORDER BY t.disc_number,t.track_number,t.id",
            )?;
            let mut rows = statement.query([id])?;
            while let Some(row) = rows.next()? {
                if tracks.len() == MAX_ALBUM_TRACKS {
                    return Err(Refusal::invalid(format!(
                        "album has more than {MAX_ALBUM_TRACKS} tracks"
                    )));
                }
                let track_id: u64 = row.get(0)?;
                let path: String = row.get(1)?;
                let metadata: String = row.get(2)?;
                let metadata: TrackMetadata = serde_json::from_str(&metadata).map_err(|e| {
                    Refusal::new(
                        ErrorCode::Persistence,
                        format!("reading the library: track {track_id} metadata: {e}"),
                    )
                })?;
                tracks.push(AlbumTrack {
                    track_id,
                    path: PathBuf::from(path),
                    title: metadata.title,
                    duration_ms: metadata.duration_ms,
                    disc_number: metadata.disc_number,
                    track_number: metadata.track_number,
                    release_id: metadata.musicbrainz_release_id,
                    recording_id: metadata.musicbrainz_recording_id,
                    release_track_id: metadata.musicbrainz_track_id,
                });
            }
        }
        tx.commit()?;
        if tracks.is_empty() {
            return Err(unknown());
        }
        // Like a release's duration: unknown if any track's is.
        let duration_ms = tracks.iter().map(|t| t.duration_ms).sum::<Option<u64>>();
        let album = AlbumFacts {
            title,
            artist,
            track_count: tracks.len(),
            duration_ms,
        };
        Ok((album, tracks))
    }

    /// Release IDs to fetch, each with its evidence (decision 6): an explicit
    /// release, else fingerprints when asked, else the stored release ID,
    /// else a search.
    fn candidates(
        &self,
        request: &LookupRequest,
        album: &AlbumFacts,
        tracks: &[AlbumTrack],
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<Vec<(String, Evidence)>, Refusal> {
        if let Some(id) = &request.release_id {
            return Ok(vec![(id.clone(), Evidence::default())]);
        }
        if request.fingerprint {
            let key = request.acoustid_key.as_deref().unwrap_or_default();
            return by_fingerprint(&self.providers, key, tracks, cancel, deadline);
        }
        if let Some(id) = most_common_release(tracks).filter(|id| is_mbid(id)) {
            return Ok(vec![(id, Evidence::default())]);
        }
        let musicbrainz = self
            .providers
            .musicbrainz
            .as_ref()
            .map_err(Refusal::lookup)?;
        let mut query = SearchQuery {
            title: &album.title,
            artist: &album.artist,
            tracks: Some(album.track_count),
        };
        cancel.check_lookup(deadline)?;
        let mut hits = musicbrainz.search(&query, cancel, deadline)?;
        if hits.is_empty() {
            // Decision 7: the second query drops the track count.
            query.tracks = None;
            cancel.check_lookup(deadline)?;
            hits = musicbrainz.search(&query, cancel, deadline)?;
        }
        let candidates: Vec<_> = hits
            .into_iter()
            .filter(|hit| is_mbid(&hit.id))
            .map(|hit| {
                let evidence = Evidence {
                    search_score: Some(hit.score),
                    votes: None,
                };
                (hit.id, evidence)
            })
            .collect();
        if candidates.is_empty() {
            return Err(Refusal::lookup(NO_RELEASE));
        }
        Ok(candidates)
    }
}

fn validate(request: &LookupRequest) -> Result<(), Refusal> {
    if let Some(id) = &request.release_id
        && !is_mbid(id)
    {
        return Err(Refusal::invalid("release_id must be a MusicBrainz ID"));
    }
    if let Some(key) = &request.acoustid_key
        && !((1..=64).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Err(Refusal::invalid(
            "acoustid_key must be 1 to 64 letters or digits",
        ));
    }
    if request.fingerprint && request.acoustid_key.is_none() {
        return Err(Refusal::invalid("fingerprint lookup needs an AcoustID key"));
    }
    Ok(())
}

/// Fingerprints every file and ranks the releases AcoustID gives for them.
/// A file that cannot be fingerprinted gives no votes.
fn by_fingerprint(
    providers: &Providers,
    key: &str,
    tracks: &[AlbumTrack],
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<Vec<(String, Evidence)>, Refusal> {
    let acoustid = providers.acoustid.as_ref().map_err(Refusal::lookup)?;
    let mut per_file = Vec::with_capacity(tracks.len());
    for track in tracks {
        cancel.check_lookup(deadline)?;
        let print = match fingerprint(&track.path, cancel, deadline) {
            Ok(Some(print)) => print,
            Ok(None) => continue,
            Err(error) => match error.downcast::<LookupError>() {
                Ok(stop) => return Err(stop.into()),
                Err(_) => continue,
            },
        };
        cancel.check_lookup(deadline)?;
        let mut ids = acoustid.lookup(key, &print, cancel, deadline)?;
        ids.retain(|id| is_mbid(id));
        per_file.push(ids);
    }
    let ranked = rank_releases(&per_file);
    if ranked.is_empty() {
        return Err(Refusal::lookup(NO_FINGERPRINT_MATCH));
    }
    Ok(ranked
        .into_iter()
        .map(|(id, votes)| {
            let evidence = Evidence {
                search_score: None,
                votes: Some(votes),
            };
            (id, evidence)
        })
        .collect())
}

/// The release, from the cache or else from MusicBrainz.
fn fetch(
    musicbrainz: &MusicBrainz,
    cache: &mut ReleaseCache,
    id: &str,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<Release, LookupError> {
    if let Some(release) = cache.get(Instant::now(), id) {
        return Ok(release);
    }
    cancel.check_lookup(deadline)?;
    let release = musicbrainz.release(id, cancel, deadline)?;
    cache.put(Instant::now(), release.clone());
    Ok(release)
}

/// The review row for one file and, when it is writable and has changes, the
/// values the proposal will write. Current values come from the file itself.
fn propose_file(
    track: &AlbumTrack,
    release: &Release,
    at: Option<TrackRef>,
) -> (TrackProposal, Option<ProposedFile>) {
    let mut row = TrackProposal {
        track_id: track.track_id,
        file_name: track
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        track_number: at
            .map(|at| release.media[at.medium].tracks[at.track].position)
            .or(track.track_number),
        matched: at.is_some(),
        writable: false,
        reason: None,
        removes_other_copies: false,
        unwritable_fields: Vec::new(),
        changes: Vec::new(),
    };
    let unreadable = |error: &dyn std::fmt::Display| {
        Some(format!("Tyranny couldn't read this file's tags: {error}"))
    };
    // A symlink or a non-regular file is refused here, not at apply.
    let stamp = match stamp(&track.path) {
        Ok(stamp) => stamp,
        Err(error) => {
            row.reason = unreadable(&error);
            return (row, None);
        }
    };
    match writable_format(&track.path) {
        Ok(Some(_)) => {}
        Ok(None) => {
            row.reason = Some(CANT_WRITE_FORMAT.into());
            return (row, None);
        }
        Err(error) => {
            row.reason = unreadable(&format!("{error:#}"));
            return (row, None);
        }
    }
    let tags = match read_tags(&track.path) {
        Ok(tags) => tags,
        Err(error) => {
            row.reason = unreadable(&format!("{error:#}"));
            return (row, None);
        }
    };
    let Some(at) = at else {
        row.reason = Some(NO_MATCHING_TRACK.into());
        return (row, None);
    };
    let unwritable = unwritable_fields_in(&tags);
    let (blocked, writable): (Vec<_>, Vec<_>) = changes(&tags, &proposed(release, at))
        .into_iter()
        .partition(|change| unwritable.contains(&change.field));
    row.unwritable_fields = blocked.into_iter().map(|change| change.field).collect();
    let changed: BTreeSet<TagField> = writable.iter().map(|change| change.field).collect();
    let copies = tags.copies_of(&changed);
    row.removes_other_copies = !copies.is_empty();
    row.changes = writable;
    if let Some(reason) = tags.read_only_reason(&changed) {
        row.reason = Some(reason.into());
        return (row, None);
    }
    row.writable = true;
    let file = (!row.changes.is_empty()).then(|| ProposedFile {
        track_id: track.track_id,
        path: track.path.clone(),
        stamp,
        values: row
            .changes
            .iter()
            .map(|change| (change.field, change.new.clone()))
            .collect(),
    });
    (row, file)
}
