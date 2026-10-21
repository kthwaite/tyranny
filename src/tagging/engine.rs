//! The worker's state, the lookup that turns an album into a stored proposal,
//! and the apply and revert that write tags.
use super::{
    ApplyRequest, Failure, LookupError, LookupRequest, MAX_ALBUM_TRACKS,
    acoustid::{AcoustId, rank_releases},
    fingerprint::fingerprint,
    journal::{FileStatus, Journal, JournalFile},
    matching::{
        AlbumFacts, AlbumTrack, Evidence, TrackRef, changes, match_files, most_common_release,
        proposed, score,
    },
    musicbrainz::{MusicBrainz, Release, SearchQuery, is_mbid},
    proposal::{Proposal, Proposals, ProposedFile, ReleaseCache},
    safe_write::{FileStamp, WriteError, stamp, write_safely},
    tags::{FileTags, TagEdit, TagKind, read_tags, unwritable_fields_in, writable_format},
};
use crate::cancel::Cancellation;
use crate::library_types::TrackMetadata;
use crate::protocol::{
    ErrorCode, FieldChange, TagApplyResult, TagCandidate, TagEditSummary, TagField, TagFileOutcome,
    TagFileResult, TagProposal, TrackProposal,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const CANT_WRITE_FORMAT: &str = "Tyranny can't write tags to this format";
const NO_MATCHING_TRACK: &str = "No matching track on this release";
const NO_RELEASE: &str = "MusicBrainz has no matching release";
const NO_FINGERPRINT_MATCH: &str = "AcoustID has no match for these files";
const LOOKUP_EXPIRED: &str = "lookup expired, look the album up again";
const NOTHING_TO_WRITE: &str = "nothing to write";
const CHANGED_SINCE_LOOKUP: &str = "changed since the lookup";
const CHANGED_SINCE_EDIT: &str = "changed since the edit";
const STOPPED: &str = "the server stopped before writing this file";
/// `tag_edits` lists this many edits.
const LISTED_EDITS: usize = 50;
/// Each number field with its total, and why the total can't be written alone.
const PAIRS: [(TagField, TagField, &str); 2] = [
    (
        TagField::TrackNumber,
        TagField::TrackTotal,
        "can't write the track total without the track number",
    ),
    (
        TagField::DiscNumber,
        TagField::DiscTotal,
        "can't write the disc total without the disc number",
    ),
];

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
/// The journal failed to open, read or record.
fn unavailable(error: anyhow::Error) -> Refusal {
    Refusal::new(
        ErrorCode::Persistence,
        format!("tag journal unavailable: {error:#}"),
    )
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

    /// `deadline` is set when the request is queued, so time spent waiting
    /// counts.
    pub(super) fn lookup(
        &mut self,
        request: &LookupRequest,
        cancel: &Cancellation,
        deadline: Instant,
    ) -> Result<TagProposal, Failure> {
        self.find(request, cancel, deadline)
            .map_err(|refusal| self.failure(refusal))
    }

    /// Writes the proposal's values to the requested files, in proposal order.
    /// `stop` is checked only between files, so a file once started is
    /// finished and journaled.
    pub(super) fn apply(
        &mut self,
        request: &ApplyRequest,
        stop: &AtomicBool,
    ) -> Result<(TagApplyResult, Vec<PathBuf>), Failure> {
        self.apply_until(request, || stop.load(Ordering::Acquire))
    }

    /// `apply`, asking `stopped` once before the journal is written and once
    /// before each file.
    pub(super) fn apply_until(
        &mut self,
        request: &ApplyRequest,
        mut stopped: impl FnMut() -> bool,
    ) -> Result<(TagApplyResult, Vec<PathBuf>), Failure> {
        self.write_proposal(request, &mut stopped)
            .map_err(|refusal| self.failure(refusal))
    }

    /// Writes each file's "before" values back, if it still holds the "after"
    /// values. `stop` is checked only between files.
    pub(super) fn revert(
        &mut self,
        edit_id: u64,
        stop: &AtomicBool,
    ) -> Result<(TagApplyResult, Vec<PathBuf>), Failure> {
        self.revert_edit(edit_id, stop)
            .map_err(|refusal| self.failure(refusal))
    }

    /// The latest journaled edits, newest first.
    pub(super) fn edits(&mut self) -> Result<Vec<TagEditSummary>, Failure> {
        let listed = self
            .journal()
            .and_then(|journal| journal.recent(LISTED_EDITS).map_err(unavailable));
        listed.map_err(|refusal| self.failure(refusal))
    }

    /// The journal, opened now if it was unavailable before.
    fn journal(&mut self) -> Result<&mut Journal, Refusal> {
        let journal = match self.journal.take() {
            Some(journal) => journal,
            None => Journal::open(&self.journal_path).map_err(unavailable)?,
        };
        Ok(self.journal.insert(journal))
    }

    fn write_proposal(
        &mut self,
        request: &ApplyRequest,
        stopped: &mut dyn FnMut() -> bool,
    ) -> Result<(TagApplyResult, Vec<PathBuf>), Refusal> {
        if request.track_ids.is_empty() {
            return Err(Refusal::invalid("choose at least one file"));
        }
        // The journal keys file statuses on the track ID.
        let mut requested = HashSet::with_capacity(request.track_ids.len());
        for id in &request.track_ids {
            if !requested.insert(*id) {
                return Err(Refusal::invalid(format!("track {id} is listed twice")));
            }
        }
        // Decision 10: from here on, a refused request has used up the proposal.
        let proposal = self
            .proposals
            .take(Instant::now(), &request.proposal_id)
            .ok_or_else(|| Refusal::invalid(LOOKUP_EXPIRED))?;
        if let Some(id) = request
            .track_ids
            .iter()
            .find(|id| !proposal.files.iter().any(|file| file.track_id == **id))
        {
            return Err(Refusal::invalid(format!(
                "track {id} is not part of this lookup"
            )));
        }
        let omitted: BTreeSet<TagField> = request.omit_fields.iter().copied().collect();
        let steps: Vec<Step> = proposal
            .files
            .iter()
            .filter(|file| requested.contains(&file.track_id))
            .map(|file| prepare(file, &omitted))
            .collect();
        let journaled: Vec<JournalFile> = steps
            .iter()
            .filter_map(|step| match step {
                Step::Write { file, .. } => Some(file.clone()),
                Step::Done(_) => None,
            })
            .collect();
        if journaled.is_empty() || stopped() {
            let files = steps
                .into_iter()
                .map(|step| match step {
                    Step::Done(result) => result,
                    Step::Write { file, .. } => stopped_result(&file),
                })
                .collect();
            return Ok((
                TagApplyResult {
                    edit_id: None,
                    files,
                },
                Vec::new(),
            ));
        }
        let journal = self.journal()?;
        let edit_id = journal
            .begin(&proposal.release_id, &proposal.release_title, &journaled)
            .map_err(unavailable)?;
        let mut files = Vec::with_capacity(steps.len());
        let mut written = Vec::new();
        for step in steps {
            let (file, expected) = match step {
                Step::Done(result) => {
                    files.push(result);
                    continue;
                }
                Step::Write { file, stamp } => (file, stamp),
            };
            if stopped() {
                logged(
                    edit_id,
                    journal.set_status(edit_id, file.track_id, FileStatus::Failed),
                );
                files.push(stopped_result(&file));
                continue;
            }
            let edit = TagEdit {
                set: file.after.clone(),
                restore: Vec::new(),
            };
            let write = write_safely(&file.path, expected, &edit);
            // The journal keeps what the write removed, so revert restores
            // exactly that. A file left `pending` is still checked by revert,
            // with the removals `prepare` predicted, so a failed update loses
            // nothing.
            let recorded = match &write {
                Ok(removed) => journal.set_written(edit_id, file.track_id, removed),
                Err(_) => journal.set_status(edit_id, file.track_id, FileStatus::Failed),
            };
            logged(edit_id, recorded);
            let (outcome, message) = match write {
                Ok(_) => (TagFileOutcome::Written, None),
                // Preflight ruling 17a: not written, so not counted and not revertible.
                Err(WriteError::Changed) => (
                    TagFileOutcome::Skipped,
                    Some(CHANGED_SINCE_LOOKUP.to_owned()),
                ),
                Err(WriteError::Failed(message)) => (TagFileOutcome::Failed, Some(message)),
            };
            if outcome == TagFileOutcome::Written {
                written.push(file.path.clone());
            }
            files.push(file_result(file.track_id, &file.path, outcome, message));
        }
        Ok((
            TagApplyResult {
                edit_id: Some(edit_id),
                files,
            },
            written,
        ))
    }

    fn revert_edit(
        &mut self,
        edit_id: u64,
        stop: &AtomicBool,
    ) -> Result<(TagApplyResult, Vec<PathBuf>), Refusal> {
        let journal = self.journal()?;
        let record = journal
            .load(edit_id)
            .map_err(unavailable)?
            .ok_or_else(|| Refusal::invalid("unknown tag edit"))?;
        if record.reverted {
            return Err(Refusal::invalid("this tag edit was already reverted"));
        }
        let mut files = Vec::with_capacity(record.files.len());
        let mut written = Vec::new();
        let mut finished = true;
        for (file, status) in record.files {
            // A `pending` file may have been written before a crash.
            if !matches!(status, FileStatus::Written | FileStatus::Pending) {
                continue;
            }
            if stop.load(Ordering::Acquire) {
                finished = false;
                files.push(stopped_result(&file));
                continue;
            }
            let (outcome, message) = match revert_file(&file) {
                Ok(()) => {
                    logged(
                        edit_id,
                        journal.set_status(edit_id, file.track_id, FileStatus::Reverted),
                    );
                    written.push(file.path.clone());
                    (TagFileOutcome::Written, None)
                }
                // Final: the file holds someone else's values now.
                Err(WriteError::Changed) => {
                    (TagFileOutcome::Skipped, Some(CHANGED_SINCE_EDIT.to_owned()))
                }
                Err(WriteError::Failed(message)) => {
                    finished = false;
                    (TagFileOutcome::Failed, Some(message))
                }
            };
            files.push(file_result(file.track_id, &file.path, outcome, message));
        }
        // A stopped or failed revert stays open, so a second revert finishes
        // it; files already reverted have status `reverted` and are skipped.
        // If the mark fails, a second revert skips every file the same way.
        if finished {
            logged(edit_id, journal.mark_reverted(edit_id));
        }
        Ok((
            TagApplyResult {
                edit_id: Some(edit_id),
                files,
            },
            written,
        ))
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
        // From a search or fingerprints, a candidate that fails to load is
        // left out. A lone candidate (explicit or stored) must load. A busy
        // service, a cancel or the deadline always stops the lookup.
        let several = candidates.len() > 1;
        let mut scored: Vec<(u32, Release)> = Vec::with_capacity(candidates.len());
        let mut first_failure = None;
        for (id, evidence) in candidates {
            match fetch(musicbrainz, &mut self.releases, &id, cancel, deadline) {
                Ok(release) => scored.push((score(&album, &release, evidence), release)),
                Err(LookupError::Failed(message)) if several => {
                    first_failure.get_or_insert(message);
                }
                Err(error) => return Err(error.into()),
            }
        }
        if scored.is_empty() {
            return Err(Refusal::lookup(
                first_failure.unwrap_or_else(|| NO_RELEASE.into()),
            ));
        }
        // Stable: on a tie, the earlier candidate comes first.
        scored.sort_by_key(|(points, _)| std::cmp::Reverse(*points));
        let release = scored[0].1.clone();

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
        file_name: file_name(&track.path),
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
    let unreadable_reason = |error: &dyn std::fmt::Display| Some(unreadable(error));
    // A symlink or a non-regular file is refused here, not at apply.
    let stamp = match stamp(&track.path) {
        Ok(stamp) => stamp,
        Err(error) => {
            row.reason = unreadable_reason(&error);
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
            row.reason = unreadable_reason(&format!("{error:#}"));
            return (row, None);
        }
    }
    let tags = match read_tags(&track.path) {
        Ok(tags) => tags,
        Err(error) => {
            row.reason = unreadable_reason(&format!("{error:#}"));
            return (row, None);
        }
    };
    let Some(at) = at else {
        row.reason = Some(NO_MATCHING_TRACK.into());
        return (row, None);
    };
    let unwritable = unwritable_fields_in(&tags);
    let values = proposed(release, at);
    let (blocked, mut writable): (Vec<_>, Vec<_>) = changes(&tags, &values)
        .into_iter()
        .partition(|change| unwritable.contains(&change.field));
    row.unwritable_fields = blocked.into_iter().map(|change| change.field).collect();
    let mut changed: BTreeSet<TagField> = writable.iter().map(|change| change.field).collect();
    // Writing one half of a number pair can drop the other half from another
    // tag. When the primary tag lacks that half, it is written there too, so
    // the review shows it and no value is lost. Its old primary value is none.
    for copy in tags.partners_dropped_with(&changed) {
        if tags.primary.contains_key(&copy.field) || !changed.insert(copy.field) {
            continue;
        }
        let new = values.get(&copy.field).cloned().unwrap_or(copy.value);
        writable.push(FieldChange {
            field: copy.field,
            old: None,
            new,
        });
    }
    writable.sort_by_key(|change| change.field);
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

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn unreadable(error: &dyn std::fmt::Display) -> String {
    format!("Tyranny couldn't read this file's tags: {error}")
}

fn file_result(
    track_id: u64,
    path: &Path,
    outcome: TagFileOutcome,
    message: Option<String>,
) -> TagFileResult {
    TagFileResult {
        track_id,
        file_name: file_name(path),
        outcome,
        message,
    }
}

/// The files are written already; a journal that can't record it is logged.
fn logged(edit_id: u64, result: anyhow::Result<()>) {
    if let Err(error) = result {
        eprintln!("tyranny: tag journal, edit {edit_id}: {error:#}");
    }
}

fn stopped_result(file: &JournalFile) -> TagFileResult {
    file_result(
        file.track_id,
        &file.path,
        TagFileOutcome::Skipped,
        Some(STOPPED.to_owned()),
    )
}

/// One requested file of an apply: already decided, or ready to write.
enum Step {
    Done(TagFileResult),
    Write { file: JournalFile, stamp: FileStamp },
}

/// Checks one proposed file and reads its current values for the journal.
fn prepare(file: &ProposedFile, omitted: &BTreeSet<TagField>) -> Step {
    let done = |outcome, message: &str| {
        Step::Done(file_result(
            file.track_id,
            &file.path,
            outcome,
            Some(message.to_owned()),
        ))
    };
    let values: BTreeMap<TagField, String> = file
        .values
        .iter()
        .filter(|(field, _)| !omitted.contains(field))
        .map(|(field, value)| (*field, value.clone()))
        .collect();
    if values.is_empty() {
        return done(TagFileOutcome::Skipped, NOTHING_TO_WRITE);
    }
    if stamp(&file.path).ok() != Some(file.stamp) {
        return done(TagFileOutcome::Skipped, CHANGED_SINCE_LOOKUP);
    }
    let tags = match read_tags(&file.path) {
        Ok(tags) => tags,
        Err(error) => return done(TagFileOutcome::Failed, &unreadable(&format!("{error:#}"))),
    };
    // A refusal, not a disk error.
    if let Some(reason) = unpaired_total(&tags, &values) {
        return done(TagFileOutcome::Skipped, reason);
    }
    let fields: BTreeSet<TagField> = values.keys().copied().collect();
    Step::Write {
        file: JournalFile {
            track_id: file.track_id,
            path: file.path.clone(),
            before: fields
                .iter()
                .map(|field| (*field, tags.primary.get(field).cloned()))
                .collect(),
            // What the write will remove; the write's own list replaces it.
            removed: [tags.copies_of(&fields), tags.partners_dropped_with(&fields)].concat(),
            after: values
                .into_iter()
                .map(|(field, value)| (field, Some(value)))
                .collect(),
        },
        stamp: file.stamp,
    }
}

/// Why a total would be written without its number, where the tag holds both
/// in one frame (ID3v2 `TRCK`) or atom (MP4 `trkn`). lofty would write the
/// number as 0 ("0/12").
fn unpaired_total(tags: &FileTags, values: &BTreeMap<TagField, String>) -> Option<&'static str> {
    if !matches!(tags.primary_kind, TagKind::Id3v2 | TagKind::Mp4Ilst) {
        return None;
    }
    PAIRS
        .iter()
        .find(|(number, total, _)| {
            values.contains_key(total)
                && !values.contains_key(number)
                && !tags.primary.contains_key(number)
        })
        .map(|(_, _, reason)| *reason)
}

/// Writes a journaled file's "before" values back, if every "after" value is
/// still its primary value.
fn revert_file(file: &JournalFile) -> Result<(), WriteError> {
    // The stamp comes first, so a change after the check fails the write.
    let expected = stamp(&file.path).map_err(|_| WriteError::Changed)?;
    let tags = read_tags(&file.path)
        .map_err(|error| WriteError::Failed(unreadable(&format!("{error:#}"))))?;
    let unchanged = file
        .after
        .iter()
        .all(|(field, value)| tags.primary.get(field) == value.as_ref());
    if !unchanged {
        return Err(WriteError::Changed);
    }
    let edit = TagEdit {
        set: file.before.clone(),
        restore: file.removed.clone(),
    };
    write_safely(&file.path, expected, &edit).map(drop)
}
