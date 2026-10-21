//! Tag fields in audio files, read and written with lofty.
//!
//! Each field has one primary home: its key in the file's primary tag. The ID3v2
//! recording ID also has a second home, written with the same value (see
//! `second_home`). Every other value of the field is a copy: an alias key in the
//! primary tag, an extra value of the primary key, or the field in another tag.
//! An edit removes every copy of each field it sets, so FFmpeg (which merges all
//! tags and accepts aliases) reads the new value.
//!
//! Every other item must survive an edit. lofty's generic `Tag` drops unmapped
//! Vorbis, APE and RIFF INFO items, mangles APE number pairs and turns AIFF
//! `COMT` into `ANNO`. So every tag is read from lofty's concrete file types:
//! - Vorbis comments, APE, RIFF INFO and AIFF text are edited key by key;
//! - ID3v2 and MP4 go through one `SplitTag`/`MergeTag` pass per edit, with raw
//!   keys edited on the split remainder;
//! - ID3v1 holds only mapped fields, so it uses the generic `Tag`.
//!
//! Every read and write goes through one open `File`, never through a path, so a
//! caller can edit a file it has verified without a link being swapped in.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use lofty::TextEncoding;
use lofty::aac::AacFile;
use lofty::ape::{ApeFile, ApeItem, ApeTag};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, Id3v2Version, TextInformationFrame};
use lofty::iff::aiff::{AiffFile, AiffTextChunks};
use lofty::iff::wav::{RiffInfoList, WavFile};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::musepack::MpcFile;
use lofty::ogg::tag::VorbisComments;
use lofty::ogg::{OggPictureStorage, OpusFile, SpeexFile, VorbisFile};
use lofty::probe::Probe;
use lofty::tag::{
    ItemKey, ItemValue, MergeTag, SplitTag, Tag, TagExt, TagItem, TagSupport, TagType,
};
use lofty::wavpack::WavPackFile;
use serde::{Deserialize, Serialize};

use crate::protocol::TagField;

/// A file type lofty can write tags to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritableFormat {
    Aac,
    Aiff,
    Ape,
    Flac,
    Mpeg,
    Mp4,
    Mpc,
    Opus,
    Vorbis,
    Speex,
    Wav,
    WavPack,
}

/// A tag type inside a file. Mirrors lofty's `TagType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagKind {
    Ape,
    Id3v1,
    Id3v2,
    Mp4Ilst,
    VorbisComments,
    RiffInfo,
    AiffText,
}

/// Where a copy of a field lives in its tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopyKey {
    /// The field's lofty `ItemKey`.
    Field,
    /// `ItemKey::Year`, an alias of `Date`.
    Year,
    /// A raw key that lofty does not map: `TXXX:<description>` in ID3v2,
    /// `----:<mean>:<name>` in MP4, the item key in Vorbis and APE. In ID3v2,
    /// `TRCK` and `TPOS` are pair frames lofty cannot parse (for example "A1").
    Raw(String),
}

/// One value of a field outside its primary home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagCopy {
    pub tag: TagKind,
    pub field: TagField,
    pub key: CopyKey,
    pub value: String,
    /// lofty can read this tag in this format but cannot write it.
    pub read_only: bool,
}

/// The reason a file is not writable when a read-only tag holds a changed field.
pub const READ_ONLY_REASON: &str = "Has an old ID3 tag Tyranny can't edit";

/// `edit_tags` fails with this (find it with `downcast_ref`) when a read-only tag
/// holds a copy of a field it would set. It displays as `READ_ONLY_REASON`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnlyTag;

impl fmt::Display for ReadOnlyTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(READ_ONLY_REASON)
    }
}

impl std::error::Error for ReadOnlyTag {}

/// The fields of one file, as lofty reads them.
#[derive(Clone, Debug)]
pub struct FileTags {
    pub format: WritableFormat,
    pub primary_kind: TagKind,
    /// The primary tag is ID3v2.3, and edits keep it v2.3.
    pub id3v23: bool,
    /// Each field's value at its primary key in the primary tag.
    pub primary: BTreeMap<TagField, String>,
    /// Every other value of a field, in lofty's tag order.
    pub copies: Vec<TagCopy>,
    /// The fields whose second home holds their primary value. That value is
    /// not a copy, so it is in neither `primary` nor `copies`.
    pub second_homes: BTreeSet<TagField>,
}

impl FileTags {
    /// The primary value, or else the first copy (decision 1).
    pub fn current(&self, field: TagField) -> Option<&str> {
        self.primary
            .get(&field)
            .or_else(|| {
                self.copies
                    .iter()
                    .find(|copy| copy.field == field)
                    .map(|copy| &copy.value)
            })
            .map(String::as_str)
    }

    pub fn copies_of(&self, fields: &BTreeSet<TagField>) -> Vec<TagCopy> {
        self.copies
            .iter()
            .filter(|copy| fields.contains(&copy.field))
            .cloned()
            .collect()
    }

    /// The copies a write of `fields` drops although it does not set them. A
    /// secondary APE tag keeps a number and its total in one item (`Track`,
    /// `Disc`), and `edit_ape` removes that item whole when the write sets
    /// either half. The other half is then lost unless the write sets it too.
    pub fn partners_dropped_with(&self, fields: &BTreeSet<TagField>) -> Vec<TagCopy> {
        let dropped = |field: TagField| {
            PAIRS.iter().any(|&(number, total)| {
                let partner = if field == number {
                    total
                } else if field == total {
                    number
                } else {
                    return false;
                };
                fields.contains(&partner) && !fields.contains(&field)
            })
        };
        self.copies
            .iter()
            .filter(|copy| {
                copy.tag == TagKind::Ape
                    && copy.tag != self.primary_kind
                    && !copy.read_only
                    && copy.key == CopyKey::Field
                    && dropped(copy.field)
            })
            .cloned()
            .collect()
    }

    /// The field's second home holds exactly its primary value, or both are
    /// empty. Always true for a field without a second home.
    pub fn second_home_agrees(&self, field: TagField) -> bool {
        let Some(key) = second_home(self.primary_kind, field) else {
            return true;
        };
        let differs = self
            .copies
            .iter()
            .any(|copy| copy.tag == self.primary_kind && copy.field == field && copy.key == key);
        !differs && self.second_homes.contains(&field) == self.primary.contains_key(&field)
    }

    /// `Some(READ_ONLY_REASON)` when setting these fields would need a change to
    /// a tag lofty cannot write. `edit_tags` then fails with `ReadOnlyTag`.
    pub fn read_only_reason(&self, fields: &BTreeSet<TagField>) -> Option<&'static str> {
        self.copies
            .iter()
            .any(|copy| copy.read_only && fields.contains(&copy.field))
            .then_some(READ_ONLY_REASON)
    }
}

/// An in-place change to one file's tags.
#[derive(Clone, Debug, Default)]
pub struct TagEdit {
    /// Each field is first removed everywhere; `Some` then writes it to its
    /// primary home, and to its second home unless `restore` puts a copy there.
    pub set: BTreeMap<TagField, Option<String>>,
    /// Copies put back into their own tags, after `set`.
    pub restore: Vec<TagCopy>,
}

/// Raw alias keys. A copy at one of these is removed when its field is set.
const ALIASES: &[(TagKind, TagField, &str)] = &[
    (
        TagKind::VorbisComments,
        TagField::MusicbrainzReleaseId,
        "MUSICBRAINZ_RELEASEID",
    ),
    (
        TagKind::VorbisComments,
        TagField::MusicbrainzRecordingId,
        "MUSICBRAINZ_RECORDINGID",
    ),
    (
        TagKind::Ape,
        TagField::MusicbrainzReleaseId,
        "MUSICBRAINZ_RELEASEID",
    ),
    (
        TagKind::Ape,
        TagField::MusicbrainzRecordingId,
        "MUSICBRAINZ_RECORDINGID",
    ),
    (TagKind::Ape, TagField::TrackTotal, "TRACKTOTAL"),
    (TagKind::Ape, TagField::TrackTotal, "TOTALTRACKS"),
    (TagKind::Ape, TagField::DiscTotal, "DISCTOTAL"),
    (TagKind::Ape, TagField::DiscTotal, "TOTALDISCS"),
    (TagKind::Ape, TagField::OriginalDate, "ORIGINALDATE"),
    (
        TagKind::Id3v2,
        TagField::MusicbrainzReleaseId,
        "TXXX:MusicBrainz Release Id",
    ),
    (TagKind::Id3v2, TagField::TrackTotal, "TXXX:TRACKTOTAL"),
    (TagKind::Id3v2, TagField::TrackTotal, "TXXX:TOTALTRACKS"),
    (TagKind::Id3v2, TagField::DiscTotal, "TXXX:DISCTOTAL"),
    (TagKind::Id3v2, TagField::DiscTotal, "TXXX:TOTALDISCS"),
    (TagKind::Id3v2, TagField::OriginalDate, "TXXX:ORIGINALYEAR"),
    (
        TagKind::Mp4Ilst,
        TagField::MusicbrainzReleaseId,
        "----:com.apple.iTunes:MusicBrainz Release Id",
    ),
    (
        TagKind::Mp4Ilst,
        TagField::MusicbrainzRecordingId,
        "----:com.apple.iTunes:MusicBrainz Recording Id",
    ),
    (
        TagKind::Mp4Ilst,
        TagField::OriginalDate,
        "----:com.apple.iTunes:ORIGINALYEAR",
    ),
];

/// The primary key is a raw key, not the field's ItemKey; the ItemKey value is
/// then a copy, unless it is the field's `second_home`.
const PRIMARY_RAW: &[(TagKind, TagField, &str)] = &[(
    TagKind::Id3v2,
    TagField::MusicbrainzRecordingId,
    "TXXX:MusicBrainz Track Id",
)];

/// The field's second home in this tag kind: a key written with the same value
/// as the primary key, for other readers. The ID3v2 recording ID lives in
/// `TXXX:MusicBrainz Track Id` and in `UFID:http://musicbrainz.org` (lofty's
/// `ItemKey` for it), which Picard, beets and mutagen read. A second-home value
/// equal to the primary value is not a copy.
fn second_home(kind: TagKind, field: TagField) -> Option<CopyKey> {
    (kind == TagKind::Id3v2 && field == TagField::MusicbrainzRecordingId).then_some(CopyKey::Field)
}

/// ID3v2.3 writes TDOR as TORY (year only), so these fields cannot round-trip in a v2.3 file.
const ID3V23_UNWRITABLE: &[TagField] = &[TagField::OriginalDate];

/// Fields that do not round-trip in this format. `tests/ffmpeg_formats.rs`
/// decides the list: every field round-trips in every writable fixture, through
/// lofty and through the library, with the dev and the release FFmpeg.
pub fn unwritable_fields(format: WritableFormat) -> &'static [TagField] {
    match format {
        WritableFormat::Aac
        | WritableFormat::Aiff
        | WritableFormat::Ape
        | WritableFormat::Flac
        | WritableFormat::Mpeg
        | WritableFormat::Mp4
        | WritableFormat::Mpc
        | WritableFormat::Opus
        | WritableFormat::Vorbis
        | WritableFormat::Speex
        | WritableFormat::Wav
        | WritableFormat::WavPack => &[],
    }
}

/// `unwritable_fields(format)` plus `ID3V23_UNWRITABLE` when `id3v23`. Lookups use this one.
pub fn unwritable_fields_in(tags: &FileTags) -> Vec<TagField> {
    let mut fields = unwritable_fields(tags.format).to_vec();
    if tags.id3v23 {
        for field in ID3V23_UNWRITABLE {
            if !fields.contains(field) {
                fields.push(*field);
            }
        }
    }
    fields
}

fn item_key(field: TagField) -> ItemKey {
    match field {
        TagField::Title => ItemKey::TrackTitle,
        TagField::Artist => ItemKey::TrackArtist,
        TagField::Album => ItemKey::AlbumTitle,
        TagField::AlbumArtist => ItemKey::AlbumArtist,
        TagField::TrackNumber => ItemKey::TrackNumber,
        TagField::TrackTotal => ItemKey::TrackTotal,
        TagField::DiscNumber => ItemKey::DiscNumber,
        TagField::DiscTotal => ItemKey::DiscTotal,
        TagField::Date => ItemKey::RecordingDate,
        TagField::OriginalDate => ItemKey::OriginalReleaseDate,
        TagField::MusicbrainzRecordingId => ItemKey::MusicBrainzRecordingId,
        TagField::MusicbrainzTrackId => ItemKey::MusicBrainzTrackId,
        TagField::MusicbrainzReleaseId => ItemKey::MusicBrainzReleaseId,
        TagField::MusicbrainzReleaseGroupId => ItemKey::MusicBrainzReleaseGroupId,
        TagField::MusicbrainzArtistId => ItemKey::MusicBrainzArtistId,
        TagField::MusicbrainzAlbumArtistId => ItemKey::MusicBrainzReleaseArtistId,
    }
}

impl TagKind {
    fn of(tag_type: TagType) -> Result<Self> {
        Ok(match tag_type {
            TagType::Ape => TagKind::Ape,
            TagType::Id3v1 => TagKind::Id3v1,
            TagType::Id3v2 => TagKind::Id3v2,
            TagType::Mp4Ilst => TagKind::Mp4Ilst,
            TagType::VorbisComments => TagKind::VorbisComments,
            TagType::RiffInfo => TagKind::RiffInfo,
            TagType::AiffText => TagKind::AiffText,
            other => bail!("unknown lofty tag type {other:?}"),
        })
    }

    fn tag_type(self) -> TagType {
        match self {
            TagKind::Ape => TagType::Ape,
            TagKind::Id3v1 => TagType::Id3v1,
            TagKind::Id3v2 => TagType::Id3v2,
            TagKind::Mp4Ilst => TagType::Mp4Ilst,
            TagKind::VorbisComments => TagType::VorbisComments,
            TagKind::RiffInfo => TagType::RiffInfo,
            TagKind::AiffText => TagType::AiffText,
        }
    }
}

/// The raw keys of a field in a tag kind: the `PRIMARY_RAW` key first, then the aliases.
fn raw_keys(kind: TagKind, field: TagField) -> impl Iterator<Item = &'static str> {
    PRIMARY_RAW
        .iter()
        .chain(ALIASES)
        .filter(move |(k, f, _)| *k == kind && *f == field)
        .map(|(_, _, key)| *key)
}

fn primary_key(kind: TagKind, field: TagField) -> CopyKey {
    PRIMARY_RAW
        .iter()
        .find(|(k, f, _)| *k == kind && *f == field)
        .map_or(CopyKey::Field, |(_, _, key)| {
            CopyKey::Raw((*key).to_owned())
        })
}

/// Track and disc numbers with their totals. APE keeps each pair in one item,
/// ID3v2 in one frame.
const PAIRS: [(TagField, TagField); 2] = [
    (TagField::TrackNumber, TagField::TrackTotal),
    (TagField::DiscNumber, TagField::DiscTotal),
];

/// The APE item and the ID3v2 frame that hold a pair, by its number field.
fn pair_keys(number: TagField) -> (&'static str, &'static str) {
    match number {
        TagField::TrackNumber => ("Track", "TRCK"),
        _ => ("Disc", "TPOS"),
    }
}

/// The field (and copy key) a lofty `ItemKey` holds, if it is one of ours.
fn field_of(key: ItemKey) -> Option<(TagField, CopyKey)> {
    if key == ItemKey::Year {
        return Some((TagField::Date, CopyKey::Year));
    }
    TagField::ALL
        .into_iter()
        .find(|field| item_key(*field) == key)
        .map(|field| (field, CopyKey::Field))
}

fn format_of(file_type: FileType) -> Option<WritableFormat> {
    Some(match file_type {
        FileType::Aac => WritableFormat::Aac,
        FileType::Aiff => WritableFormat::Aiff,
        FileType::Ape => WritableFormat::Ape,
        FileType::Flac => WritableFormat::Flac,
        FileType::Mpeg => WritableFormat::Mpeg,
        FileType::Mp4 => WritableFormat::Mp4,
        FileType::Mpc => WritableFormat::Mpc,
        FileType::Opus => WritableFormat::Opus,
        FileType::Vorbis => WritableFormat::Vorbis,
        FileType::Speex => WritableFormat::Speex,
        FileType::Wav => WritableFormat::Wav,
        FileType::WavPack => WritableFormat::WavPack,
        _ => return None,
    })
}

/// Detects the file type from its content, never from its extension.
///
/// lofty's `guess_file_type` searches up to 1 KiB of leading junk for an MPEG
/// or ADTS frame sync, and so takes AC-3, AU, CAF, DTS, Matroska, Ogg FLAC,
/// RealAudio and TTA files for MP3 or AAC. Only a type lofty determines from the
/// file's first bytes, or from the bytes after a leading ID3v2 tag, counts.
pub fn writable_format(path: &Path) -> Result<Option<WritableFormat>> {
    writable_format_file(&mut File::open(path)?)
}

fn writable_format_file(file: &mut File) -> Result<Option<WritableFormat>> {
    file.rewind()?;
    let mut head = Vec::with_capacity(36);
    file.by_ref().take(36).read_to_end(&mut head)?;
    file.rewind()?;
    let file_type = if head.starts_with(b"ID3") {
        Probe::new(BufReader::new(&mut *file))
            .guess_file_type()?
            .file_type()
    } else {
        FileType::from_buffer(&head)
    };
    let Some(format) = file_type.and_then(format_of) else {
        return Ok(None);
    };
    // A QuickTime movie: its `ftyp` major brand is `qt  ` (decision 4).
    if format == WritableFormat::Mp4 && head.get(8..12) == Some(b"qt  ".as_slice()) {
        return Ok(None);
    }
    Ok(Some(format))
}

/// Opens the file once and reads it with `read_tags_file`.
pub fn read_tags(path: &Path) -> Result<FileTags> {
    let mut file = File::open(path)?;
    read_tags_file(&mut file).with_context(|| format!("reading tags of {}", path.display()))
}

/// Reads the tags through this handle only.
pub fn read_tags_file(file: &mut File) -> Result<FileTags> {
    Parsed::read(file)?.file_tags()
}

/// Opens the file once for reading and writing, and edits it with `edit_tags_file`.
pub fn edit_tags(path: &Path, edit: &TagEdit) -> Result<Vec<TagCopy>> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    edit_tags_file(&mut file, edit).with_context(|| format!("writing tags to {}", path.display()))
}

/// Edits the file in place through this handle only, which must be open for
/// reading and writing. Returns the copies it removed, so that `restore` can put
/// them back. Fails with `ReadOnlyTag` before any change when a read-only tag
/// holds a copy of a field in `set`.
pub fn edit_tags_file(file: &mut File, edit: &TagEdit) -> Result<Vec<TagCopy>> {
    let mut parsed = Parsed::read(file)?;
    let removed = parsed.apply(edit)?;
    parsed.save(file)?;
    Ok(removed)
}

/// A file's tags, each in the form it is edited in.
struct Parsed {
    format: WritableFormat,
    file_type: FileType,
    id3v23: bool,
    /// In lofty's tag order.
    slots: Vec<Slot>,
}

struct Slot {
    kind: TagKind,
    read_only: bool,
    body: Body,
    modified: bool,
}

enum Body {
    /// ID3v1 holds only mapped fields, so the generic tag loses nothing.
    Id3v1(Tag),
    Id3v2(Id3v2Tag),
    Ilst(Ilst),
    Vorbis(VorbisComments),
    Ape(ApeTag),
    Riff(RiffInfoList),
    Aiff(AiffTextChunks),
}

/// A value to add to a tag.
type Push = (TagField, CopyKey, String);

/// The fields' values in one tag, in the order they are found.
type Entries = BTreeMap<TagField, Vec<(CopyKey, String)>>;

impl Parsed {
    fn read(file: &mut File) -> Result<Self> {
        let format = writable_format_file(file)?
            .ok_or_else(|| anyhow!("Tyranny can't write tags to this format"))?;
        file.rewind()?;
        let tagged = Probe::new(BufReader::new(&mut *file))
            .guess_file_type()?
            .read()?;
        let file_type = tagged.file_type();
        ensure!(
            format_of(file_type) == Some(format),
            "lofty read {file_type:?}, not {format:?}"
        );
        file.rewind()?;
        let mut concrete = Concrete::read(&mut BufReader::new(&mut *file), file_type)?;
        let mut slots = Vec::new();
        for tag in tagged.tags() {
            let kind = TagKind::of(tag.tag_type())?;
            slots.push(Slot::new(file_type, kind, concrete.take(kind, tag)?));
        }
        for (kind, body) in concrete.rest() {
            slots.push(Slot::new(file_type, kind, body));
        }
        let id3v23 = file_type.primary_tag_type() == TagType::Id3v2
            && slots.iter().any(|slot| {
                matches!(&slot.body, Body::Id3v2(tag) if tag.original_version() == Id3v2Version::V3)
            });
        Ok(Self {
            format,
            file_type,
            id3v23,
            slots,
        })
    }

    fn primary_kind(&self) -> Result<TagKind> {
        TagKind::of(self.file_type.primary_tag_type())
    }

    fn file_tags(&self) -> Result<FileTags> {
        let primary_kind = self.primary_kind()?;
        let mut primary = BTreeMap::new();
        let mut copies = Vec::new();
        let mut second_homes = BTreeSet::new();
        for slot in &self.slots {
            for (field, mut entries) in slot.body.entries() {
                if slot.kind == primary_kind {
                    let key = primary_key(slot.kind, field);
                    if let Some(position) = entries.iter().position(|(k, _)| *k == key) {
                        let value = entries.remove(position).1;
                        if let Some(second) = second_home(slot.kind, field)
                            && let Some(position) = entries
                                .iter()
                                .position(|(k, v)| *k == second && *v == value)
                        {
                            entries.remove(position);
                            second_homes.insert(field);
                        }
                        primary.insert(field, value);
                    }
                }
                copies.extend(entries.into_iter().map(|(key, value)| TagCopy {
                    tag: slot.kind,
                    field,
                    key,
                    value,
                    read_only: slot.read_only,
                }));
            }
        }
        Ok(FileTags {
            format: self.format,
            primary_kind,
            id3v23: self.id3v23,
            primary,
            copies,
            second_homes,
        })
    }

    /// Applies the edit in memory. Returns the copies it removes: the copies of
    /// the fields in `set`, and the partners of a number pair that had to go
    /// with them.
    fn apply(&mut self, edit: &TagEdit) -> Result<Vec<TagCopy>> {
        let fields: BTreeSet<TagField> = edit.set.keys().copied().collect();
        let tags = self.file_tags()?;
        if tags.read_only_reason(&fields).is_some() || edit.restore.iter().any(|c| c.read_only) {
            return Err(ReadOnlyTag.into());
        }
        let mut removed = tags.copies_of(&fields);
        let primary_kind = self.primary_kind()?;
        let mut pushes: BTreeMap<usize, Vec<Push>> = BTreeMap::new();
        for (field, value) in &edit.set {
            if let Some(value) = value {
                let slot = self.slot(primary_kind)?;
                let push = pushes.entry(slot).or_default();
                push.push((*field, primary_key(primary_kind, *field), value.clone()));
                // A restored copy at the second home takes its place.
                if let Some(second) = second_home(primary_kind, *field)
                    && !edit.restore.iter().any(|copy| {
                        copy.tag == primary_kind && copy.field == *field && copy.key == second
                    })
                {
                    push.push((*field, second, value.clone()));
                }
            }
        }
        for copy in &edit.restore {
            let slot = self.slot(copy.tag)?;
            pushes.entry(slot).or_default().push((
                copy.field,
                copy.key.clone(),
                copy.value.clone(),
            ));
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.read_only {
                continue;
            }
            let push = pushes.remove(&index).unwrap_or_default();
            let primary = slot.kind == primary_kind;
            let kind = slot.kind;
            let dropped = slot.edit(&fields, &push, primary)?;
            removed.extend(dropped.into_iter().map(|(field, key, value)| TagCopy {
                tag: kind,
                field,
                key,
                value,
                read_only: false,
            }));
        }
        Ok(removed)
    }

    /// The index of the writable slot of this kind, created when absent.
    fn slot(&mut self, kind: TagKind) -> Result<usize> {
        if !self.file_type.tag_support(kind.tag_type()).is_writable() {
            bail!("lofty can't write {kind:?} tags to {:?} files", self.format);
        }
        if let Some(position) = self.slots.iter().position(|slot| slot.kind == kind) {
            return Ok(position);
        }
        self.slots
            .push(Slot::new(self.file_type, kind, Body::new(kind)));
        Ok(self.slots.len() - 1)
    }

    fn save(&self, file: &mut File) -> Result<()> {
        let mut options = WriteOptions::default();
        if self.id3v23 {
            options = options.use_id3v23(true);
        }
        for slot in self.slots.iter().filter(|slot| slot.modified) {
            file.rewind()?;
            slot.body
                .save(file, options)
                .with_context(|| format!("saving the {:?} tag", slot.kind))?;
        }
        Ok(())
    }
}

/// The tags of a file, from lofty's concrete file type, which keeps every item.
#[derive(Default)]
struct Concrete {
    id3v2: Option<Id3v2Tag>,
    ilst: Option<Ilst>,
    vorbis: Option<VorbisComments>,
    ape: Option<ApeTag>,
    riff: Option<RiffInfoList>,
    aiff: Option<AiffTextChunks>,
}

impl Concrete {
    fn read(reader: &mut BufReader<&mut File>, file_type: FileType) -> Result<Self> {
        let options = ParseOptions::new();
        let mut tags = Concrete::default();
        match file_type {
            FileType::Aac => tags.id3v2 = AacFile::read_from(reader, options)?.remove_id3v2(),
            FileType::Aiff => {
                let mut file = AiffFile::read_from(reader, options)?;
                tags.id3v2 = file.remove_id3v2();
                tags.aiff = file.remove_text_chunks();
            }
            FileType::Ape => {
                let mut file = ApeFile::read_from(reader, options)?;
                tags.id3v2 = file.remove_id3v2();
                tags.ape = file.remove_ape();
            }
            FileType::Flac => {
                // Saving Vorbis comments to FLAC rewrites every PICTURE block
                // from the tag's pictures, so the file's pictures move there.
                let mut file = FlacFile::read_from(reader, options)?;
                tags.id3v2 = file.remove_id3v2();
                let pictures = file.remove_pictures();
                tags.vorbis = file.remove_vorbis_comments();
                if !pictures.is_empty() {
                    let comments = tags.vorbis.get_or_insert_with(VorbisComments::new);
                    for (picture, information) in pictures {
                        comments.insert_picture(picture, Some(information))?;
                    }
                }
            }
            FileType::Mpeg => {
                let mut file = MpegFile::read_from(reader, options)?;
                tags.id3v2 = file.remove_id3v2();
                tags.ape = file.remove_ape();
            }
            FileType::Mp4 => tags.ilst = Mp4File::read_from(reader, options)?.remove_ilst(),
            FileType::Mpc => {
                let mut file = MpcFile::read_from(reader, options)?;
                tags.id3v2 = file.remove_id3v2();
                tags.ape = file.remove_ape();
            }
            FileType::Opus => {
                tags.vorbis = Some(OpusFile::read_from(reader, options)?.remove_vorbis_comments());
            }
            FileType::Vorbis => {
                tags.vorbis =
                    Some(VorbisFile::read_from(reader, options)?.remove_vorbis_comments());
            }
            FileType::Speex => {
                tags.vorbis = Some(SpeexFile::read_from(reader, options)?.remove_vorbis_comments());
            }
            FileType::Wav => {
                let mut file = WavFile::read_from(reader, options)?;
                tags.id3v2 = file.remove_id3v2();
                tags.riff = file.remove_riff_info();
            }
            FileType::WavPack => tags.ape = WavPackFile::read_from(reader, options)?.remove_ape(),
            other => bail!("lofty can't write {other:?} files"),
        }
        Ok(tags)
    }

    /// The concrete tag lofty listed as `generic`.
    fn take(&mut self, kind: TagKind, generic: &Tag) -> Result<Body> {
        let missing = || format!("lofty listed a {kind:?} tag that its file type lacks");
        Ok(match kind {
            TagKind::Id3v1 => Body::Id3v1(generic.clone()),
            TagKind::Id3v2 => Body::Id3v2(self.id3v2.take().with_context(missing)?),
            TagKind::Mp4Ilst => Body::Ilst(self.ilst.take().with_context(missing)?),
            TagKind::VorbisComments => Body::Vorbis(self.vorbis.take().with_context(missing)?),
            TagKind::Ape => Body::Ape(self.ape.take().with_context(missing)?),
            TagKind::RiffInfo => Body::Riff(self.riff.take().with_context(missing)?),
            TagKind::AiffText => Body::Aiff(self.aiff.take().with_context(missing)?),
        })
    }

    /// Tags lofty did not list.
    fn rest(self) -> impl Iterator<Item = (TagKind, Body)> {
        [
            self.id3v2.map(|tag| (TagKind::Id3v2, Body::Id3v2(tag))),
            self.ilst.map(|tag| (TagKind::Mp4Ilst, Body::Ilst(tag))),
            self.vorbis
                .map(|tag| (TagKind::VorbisComments, Body::Vorbis(tag))),
            self.ape.map(|tag| (TagKind::Ape, Body::Ape(tag))),
            self.riff.map(|tag| (TagKind::RiffInfo, Body::Riff(tag))),
            self.aiff.map(|tag| (TagKind::AiffText, Body::Aiff(tag))),
        ]
        .into_iter()
        .flatten()
    }
}

impl Slot {
    fn new(file_type: FileType, kind: TagKind, body: Body) -> Self {
        Self {
            kind,
            read_only: file_type.tag_support(kind.tag_type()) == TagSupport::ReadOnly,
            body,
            modified: false,
        }
    }

    /// Removes every value of each field in `remove`, then adds `push`. Returns
    /// the values of other fields it had to drop.
    fn edit(
        &mut self,
        remove: &BTreeSet<TagField>,
        push: &[Push],
        primary: bool,
    ) -> Result<Vec<Push>> {
        let mut dropped = Vec::new();
        let changed = match &mut self.body {
            Body::Id3v1(tag) => edit_generic(tag, remove, push)?,
            Body::Id3v2(tag) => edit_id3v2(tag, remove, push, &mut dropped)?,
            Body::Ilst(tag) => edit_ilst(tag, remove, push)?,
            Body::Vorbis(tag) => edit_vorbis(tag, remove, push),
            Body::Ape(tag) => edit_ape(tag, remove, push, primary, &mut dropped)?,
            Body::Riff(tag) => edit_riff(tag, remove, push)?,
            Body::Aiff(tag) => edit_aiff(tag, remove, push)?,
        };
        self.modified |= changed;
        Ok(dropped)
    }
}

impl Body {
    fn new(kind: TagKind) -> Self {
        match kind {
            TagKind::Id3v1 => Body::Id3v1(Tag::new(TagType::Id3v1)),
            TagKind::Id3v2 => Body::Id3v2(Id3v2Tag::new()),
            TagKind::Mp4Ilst => Body::Ilst(Ilst::new()),
            TagKind::VorbisComments => Body::Vorbis(VorbisComments::new()),
            TagKind::Ape => Body::Ape(ApeTag::new()),
            TagKind::RiffInfo => Body::Riff(RiffInfoList::new()),
            TagKind::AiffText => Body::Aiff(AiffTextChunks::new()),
        }
    }

    fn save(&self, file: &mut File, options: WriteOptions) -> Result<()> {
        match self {
            Body::Id3v1(tag) => tag.save_to(file, options),
            Body::Id3v2(tag) => tag.save_to(file, options),
            Body::Ilst(tag) => tag.save_to(file, options),
            Body::Vorbis(tag) => tag.save_to(file, options),
            Body::Ape(tag) => tag.save_to(file, options),
            Body::Riff(tag) => tag.save_to(file, options),
            Body::Aiff(tag) => tag.save_to(file, options),
        }?;
        Ok(())
    }

    /// Every text value of every field in this tag. Within a field: the mapped
    /// values (the ItemKey, and `Year` for `Date`), then the raw keys. Binary
    /// values are never fields, so edits leave them alone.
    fn entries(&self) -> Entries {
        let mut entries = Entries::new();
        let mut add = |field: TagField, key: CopyKey, value: &str| {
            entries
                .entry(field)
                .or_default()
                .push((key, value.to_owned()));
        };
        match self {
            Body::Id3v1(tag) => add_items(tag, &mut add),
            Body::Id3v2(tag) => {
                let mut tag = tag.clone();
                let unparsed = take_unparsed_pairs(&mut tag);
                let (rest, items) = tag.split_tag();
                add_items(&items, &mut add);
                for field in TagField::ALL {
                    for key in raw_keys(TagKind::Id3v2, field) {
                        for value in user_texts(&rest, txxx(key)) {
                            add(field, CopyKey::Raw(key.to_owned()), &value);
                        }
                    }
                }
                for (number, value) in unparsed {
                    add(number, CopyKey::Raw(pair_keys(number).1.to_owned()), &value);
                }
            }
            Body::Ilst(tag) => {
                let (rest, items) = tag.clone().split_tag();
                add_items(&items, &mut add);
                for field in TagField::ALL {
                    for key in raw_keys(TagKind::Mp4Ilst, field) {
                        for value in freeform_texts(&rest, key) {
                            add(field, CopyKey::Raw(key.to_owned()), &value);
                        }
                    }
                }
            }
            Body::Vorbis(tag) => {
                for (key, value) in tag.items() {
                    if let Some((field, copy_key)) = vorbis_field(key) {
                        add(field, copy_key, value);
                    }
                }
                for field in TagField::ALL {
                    for key in raw_keys(TagKind::VorbisComments, field) {
                        for value in tag.get_all(key) {
                            add(field, CopyKey::Raw(key.to_owned()), value);
                        }
                    }
                }
            }
            Body::Ape(tag) => {
                for item in tag {
                    let Some(text) = item.value().text() else {
                        continue;
                    };
                    if let Some((number, total)) = ape_pair(item.key()) {
                        for value in text.split('\0') {
                            let (n, t) = pair_parts(value);
                            if let Some(n) = n {
                                add(number, CopyKey::Field, n);
                            }
                            if let Some(t) = t {
                                add(total, CopyKey::Field, t);
                            }
                        }
                    } else if let Some((field, key)) =
                        ItemKey::from_key(TagType::Ape, item.key()).and_then(field_of)
                    {
                        for value in text.split('\0') {
                            add(field, key.clone(), value);
                        }
                    }
                }
                for field in TagField::ALL {
                    for key in raw_keys(TagKind::Ape, field) {
                        if let Some(text) = tag.get(key).and_then(|item| item.value().text()) {
                            for value in text.split('\0') {
                                add(field, CopyKey::Raw(key.to_owned()), value);
                            }
                        }
                    }
                }
            }
            Body::Riff(tag) => {
                for (key, value) in tag.clone() {
                    if let Some((field, copy_key)) =
                        ItemKey::from_key(TagType::RiffInfo, &key).and_then(field_of)
                    {
                        add(field, copy_key, &value);
                    }
                }
            }
            Body::Aiff(tag) => {
                if let Some(name) = &tag.name {
                    add(TagField::Title, CopyKey::Field, name);
                }
                if let Some(author) = &tag.author {
                    add(TagField::Artist, CopyKey::Field, author);
                }
            }
        }
        entries
    }
}

fn add_items(items: &Tag, add: &mut impl FnMut(TagField, CopyKey, &str)) {
    for field in TagField::ALL {
        for value in items.get_strings(item_key(field)) {
            add(field, CopyKey::Field, value);
        }
        if field == TagField::Date {
            for value in items.get_strings(ItemKey::Year) {
                add(field, CopyKey::Year, value);
            }
        }
    }
}

fn vorbis_field(key: &str) -> Option<(TagField, CopyKey)> {
    ItemKey::from_key(TagType::VorbisComments, key).and_then(field_of)
}

/// The number and total fields of an APE pair item.
fn ape_pair(key: &str) -> Option<(TagField, TagField)> {
    PAIRS
        .into_iter()
        .find(|(number, _)| pair_keys(*number).0.eq_ignore_ascii_case(key))
}

/// "3/12" → ("3", "12"); "3" → ("3", -); "/12" → (-, "12").
fn pair_parts(value: &str) -> (Option<&str>, Option<&str>) {
    fn present(part: &str) -> Option<&str> {
        Some(part.trim()).filter(|part| !part.is_empty())
    }
    match value.split_once('/') {
        Some((number, total)) => (present(number), present(total)),
        None => (present(value), None),
    }
}

/// Removes the mapped values of the fields from a generic tag.
fn remove_items(items: &mut Tag, remove: &BTreeSet<TagField>) -> bool {
    let before = items.item_count();
    for field in remove {
        items.remove_key(item_key(*field));
        if *field == TagField::Date {
            items.remove_key(ItemKey::Year);
        }
    }
    items.item_count() != before
}

/// Adds a mapped value to a generic tag.
fn push_item(items: &mut Tag, field: TagField, key: &CopyKey, value: &str) -> Result<()> {
    let item_key = match key {
        CopyKey::Field => item_key(field),
        CopyKey::Year => ItemKey::Year,
        CopyKey::Raw(raw) => bail!("{:?} tags have no raw key {raw}", items.tag_type()),
    };
    if !items.push(TagItem::new(item_key, ItemValue::Text(value.to_owned()))) {
        bail!(
            "lofty can't store {item_key:?} in a {:?} tag",
            items.tag_type()
        );
    }
    Ok(())
}

fn edit_generic(tag: &mut Tag, remove: &BTreeSet<TagField>, push: &[Push]) -> Result<bool> {
    let mut changed = remove_items(tag, remove);
    for (field, key, value) in push {
        push_item(tag, *field, key, value)?;
        changed = true;
    }
    Ok(changed)
}

/// One split/merge pass. Raw keys are edited on the split remainder, which
/// holds every frame lofty does not map.
fn edit_id3v2(
    tag: &mut Id3v2Tag,
    remove: &BTreeSet<TagField>,
    push: &[Push],
    dropped: &mut Vec<Push>,
) -> Result<bool> {
    // A pair frame lofty can't parse ("A1") is the number field's raw copy. Its
    // split would make it an item that merging then drops, so it stays aside.
    let mut unparsed = take_unparsed_pairs(tag);
    let (rest, mut items) = std::mem::take(tag).split_tag();
    let mut frames = Id3v2Tag::from(rest);
    let mut changed = remove_items(&mut items, remove);
    for field in remove {
        for key in raw_keys(TagKind::Id3v2, *field) {
            while frames.remove_user_text(txxx(key)).is_some() {
                changed = true;
            }
        }
    }
    for (number, total) in PAIRS {
        if !remove.contains(&number) && !remove.contains(&total) {
            continue;
        }
        let (gone, kept): (Vec<_>, Vec<_>) = unparsed
            .into_iter()
            .partition(|(field, _)| *field == number);
        unparsed = kept;
        changed |= !gone.is_empty();
        if !remove.contains(&number) {
            let id = pair_keys(number).1;
            dropped.extend(
                gone.into_iter()
                    .map(|(field, value)| (field, CopyKey::Raw(id.to_owned()), value)),
            );
        }
    }
    for (field, key, value) in push {
        changed = true;
        match key {
            CopyKey::Raw(id) if PAIRS.iter().any(|(number, _)| pair_keys(*number).1 == id) => {
                unparsed.push((*field, value.clone()));
            }
            CopyKey::Raw(raw) => {
                let description = txxx(raw);
                let content = match frames.get_user_text(description) {
                    Some(existing) => format!("{existing}\0{value}"),
                    None => value.clone(),
                };
                frames.insert_user_text(description.to_owned(), content);
            }
            _ => push_item(&mut items, *field, key, value)?,
        }
    }
    // lofty writes a total without its number as "0/12", which reads back as
    // track number "0". Write a total only with its number.
    for (number, total) in PAIRS {
        let has = |field: TagField| items.get(item_key(field)).is_some();
        if has(total) && !has(number) {
            if push.iter().any(|(field, _, _)| *field == total) {
                bail!("an ID3v2 tag can't hold {total:?} without {number:?}");
            }
            let values: Vec<String> = items.take_strings(item_key(total)).collect();
            dropped.extend(
                values
                    .into_iter()
                    .map(|value| (total, CopyKey::Field, value)),
            );
            changed = true;
        }
    }
    let (rest, leftover) = frames.split_tag();
    for item in leftover.items() {
        items.push(item.clone());
    }
    *tag = rest.merge_tag(items);
    for (number, value) in unparsed {
        tag.insert(pair_frame(pair_keys(number).1, value));
    }
    Ok(changed)
}

fn pair_frame(id: &str, value: String) -> Frame<'static> {
    Frame::Text(TextInformationFrame::new(
        FrameId::Valid(Cow::Owned(id.to_owned())),
        TextEncoding::UTF8,
        value,
    ))
}

/// Takes out the `TRCK`/`TPOS` frames that lofty's split can't read as a
/// number pair. Returns their values by number field.
fn take_unparsed_pairs(tag: &mut Id3v2Tag) -> Vec<(TagField, String)> {
    let mut unparsed = Vec::new();
    for (number, _) in PAIRS {
        let id = pair_keys(number).1;
        let values: Vec<String> = (&*tag)
            .into_iter()
            .filter_map(|frame| match frame {
                Frame::Text(text) if frame.id_str() == id && !lofty_pair(&text.value) => {
                    Some(text.value.to_string())
                }
                _ => None,
            })
            .collect();
        if !values.is_empty() {
            tag.retain(|frame| {
                !matches!(frame, Frame::Text(text) if frame.id_str() == id && !lofty_pair(&text.value))
            });
            unparsed.extend(values.into_iter().map(|value| (number, value)));
        }
    }
    unparsed
}

/// Whether lofty's ID3v2 split reads this as a number pair: "3", "3/12".
fn lofty_pair(value: &str) -> bool {
    let mut parts = value.splitn(2, ['\0', '/']);
    let number = |part: Option<&str>| part.is_some_and(|part| part.trim().parse::<u32>().is_ok());
    let first = parts.next();
    let second = parts.next();
    number(first) && (second.is_none() || number(second))
}

fn user_texts(tag: &Id3v2Tag, description: &str) -> Vec<String> {
    tag.into_iter()
        .filter_map(|frame| match frame {
            Frame::UserText(text) if text.description == description => {
                Some(text.content.split('\0').map(str::to_owned))
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// One split/merge pass. Raw freeform atoms are edited on the split remainder.
/// Only their text data is a field; binary data stays.
fn edit_ilst(tag: &mut Ilst, remove: &BTreeSet<TagField>, push: &[Push]) -> Result<bool> {
    let (rest, mut items) = std::mem::take(tag).split_tag();
    let mut atoms = Ilst::from(rest);
    let mut changed = remove_items(&mut items, remove);
    for field in remove {
        for key in raw_keys(TagKind::Mp4Ilst, *field) {
            let ident = freeform(key);
            let old: Vec<Atom<'static>> = atoms.remove(&ident).collect();
            for atom in old {
                let (text, other): (Vec<AtomData>, Vec<AtomData>) = atom
                    .into_data()
                    .partition(|data| matches!(data, AtomData::UTF8(_) | AtomData::UTF16(_)));
                changed |= !text.is_empty();
                if let Some(atom) = Atom::from_collection(ident.clone(), other) {
                    atoms.insert(atom);
                }
            }
        }
    }
    for (field, key, value) in push {
        changed = true;
        match key {
            CopyKey::Raw(raw) => {
                atoms.insert(Atom::new(freeform(raw), AtomData::UTF8(value.clone())));
            }
            _ => push_item(&mut items, *field, key, value)?,
        }
    }
    let (rest, leftover) = atoms.split_tag();
    for item in leftover.items() {
        items.push(item.clone());
    }
    *tag = rest.merge_tag(items);
    Ok(changed)
}

fn freeform_texts(tag: &Ilst, key: &str) -> Vec<String> {
    tag.get(&freeform(key))
        .into_iter()
        .flat_map(Atom::data)
        .filter_map(|data| match data {
            AtomData::UTF8(text) | AtomData::UTF16(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Edits key by key; every other comment stays as it is.
fn edit_vorbis(tag: &mut VorbisComments, remove: &BTreeSet<TagField>, push: &[Push]) -> bool {
    let mut keys: Vec<String> = tag
        .items()
        .filter(|(key, _)| vorbis_field(key).is_some_and(|(field, _)| remove.contains(&field)))
        .map(|(key, _)| key.to_owned())
        .collect();
    for field in remove {
        keys.extend(raw_keys(TagKind::VorbisComments, *field).map(str::to_owned));
    }
    let mut changed = false;
    for key in keys {
        changed |= tag.remove(&key).count() > 0;
    }
    for (field, key, value) in push {
        let name = match key {
            CopyKey::Field => item_key(*field).map_key(TagType::VorbisComments),
            CopyKey::Year => ItemKey::Year.map_key(TagType::VorbisComments),
            CopyKey::Raw(raw) => Some(raw.as_str()),
        };
        if let Some(name) = name {
            tag.push(name.to_owned(), value.clone());
            changed = true;
        }
    }
    changed
}

/// Edits key by key. A number and its total share one item (`Track`, `Disc`),
/// so that item is rebuilt from the values that stay and the values pushed. In
/// a secondary tag the partner of a removed field goes too, and is returned so
/// the journal can restore it.
fn edit_ape(
    tag: &mut ApeTag,
    remove: &BTreeSet<TagField>,
    push: &[Push],
    primary: bool,
    dropped: &mut Vec<Push>,
) -> Result<bool> {
    let mut changed = false;
    for (number, total) in PAIRS {
        let (key, _) = pair_keys(number);
        let pushed = |field: TagField| {
            push.iter()
                .filter(move |(f, k, _)| *f == field && *k == CopyKey::Field)
                .map(|(_, _, value)| value.clone())
        };
        let touched = remove.contains(&number)
            || remove.contains(&total)
            || pushed(number).next().is_some()
            || pushed(total).next().is_some();
        if !touched {
            continue;
        }
        let (mut numbers, mut totals) = (Vec::new(), Vec::new());
        if let Some(item) = tag.get(key) {
            let text = item
                .value()
                .text()
                .with_context(|| format!("the APE {key} item is not text"))?;
            for value in text.split('\0') {
                let (n, t) = pair_parts(value);
                numbers.extend(n.map(str::to_owned));
                totals.extend(t.map(str::to_owned));
            }
            tag.remove(key);
            changed = true;
        }
        for (field, partner, values) in
            [(number, total, &mut numbers), (total, number, &mut totals)]
        {
            if remove.contains(&field) {
                values.clear();
            } else if remove.contains(&partner) && !primary {
                dropped.extend(values.drain(..).map(|value| (field, CopyKey::Field, value)));
            }
            values.extend(pushed(field));
        }
        let joined = (0..numbers.len().max(totals.len()))
            .map(|index| match (numbers.get(index), totals.get(index)) {
                (Some(n), Some(t)) => format!("{n}/{t}"),
                (Some(n), None) => n.clone(),
                (None, Some(t)) => format!("/{t}"),
                (None, None) => unreachable!("index is below one of the lengths"),
            })
            .collect::<Vec<_>>()
            .join("\0");
        if !joined.is_empty() {
            tag.insert(ApeItem::new(key.to_owned(), ItemValue::Text(joined))?);
            changed = true;
        }
    }
    let mut keys: Vec<String> = (&*tag)
        .into_iter()
        .filter(|item| item.value().text().is_some() && ape_pair(item.key()).is_none())
        .filter(|item| {
            ItemKey::from_key(TagType::Ape, item.key())
                .and_then(field_of)
                .is_some_and(|(field, _)| remove.contains(&field))
        })
        .map(|item| item.key().to_owned())
        .collect();
    for field in remove {
        for key in raw_keys(TagKind::Ape, *field) {
            if tag
                .get(key)
                .is_some_and(|item| item.value().text().is_some())
            {
                keys.push(key.to_owned());
            }
        }
    }
    for key in keys {
        tag.remove(&key);
        changed = true;
    }
    for (field, key, value) in push {
        let name = match key {
            CopyKey::Field if PAIRS.iter().any(|(n, t)| n == field || t == field) => continue,
            CopyKey::Field => item_key(*field).map_key(TagType::Ape),
            CopyKey::Year => ItemKey::Year.map_key(TagType::Ape),
            CopyKey::Raw(raw) => Some(raw.as_str()),
        }
        .with_context(|| format!("APE has no key for {field:?}"))?;
        if tag
            .get(name)
            .is_some_and(|item| item.value().text().is_none())
        {
            bail!("the APE {name} item holds binary data");
        }
        tag.push(ApeItem::new(
            name.to_owned(),
            ItemValue::Text(value.clone()),
        )?);
        changed = true;
    }
    Ok(changed)
}

/// Edits key by key; every other INFO item stays as it is.
fn edit_riff(tag: &mut RiffInfoList, remove: &BTreeSet<TagField>, push: &[Push]) -> Result<bool> {
    let mut keys: Vec<String> = tag
        .clone()
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| {
            ItemKey::from_key(TagType::RiffInfo, key)
                .and_then(field_of)
                .is_some_and(|(field, _)| remove.contains(&field))
        })
        .collect();
    keys.dedup();
    let mut changed = false;
    for key in keys {
        while tag.remove(&key).is_some() {
            changed = true;
        }
    }
    for (field, key, value) in push {
        let name = match key {
            CopyKey::Field => item_key(*field).map_key(TagType::RiffInfo),
            _ => None,
        }
        .with_context(|| format!("RIFF INFO has no key for {field:?} at {key:?}"))?;
        tag.insert(name.to_owned(), value.clone());
        changed = true;
    }
    Ok(changed)
}

/// Edits the name and author chunks; annotations and comments (with their
/// timestamps and marker IDs) stay as they are.
fn edit_aiff(tag: &mut AiffTextChunks, remove: &BTreeSet<TagField>, push: &[Push]) -> Result<bool> {
    let mut changed = false;
    if remove.contains(&TagField::Title) {
        changed |= tag.name.take().is_some();
    }
    if remove.contains(&TagField::Artist) {
        changed |= tag.author.take().is_some();
    }
    for (field, key, value) in push {
        let chunk = match (field, key) {
            (TagField::Title, CopyKey::Field) => &mut tag.name,
            (TagField::Artist, CopyKey::Field) => &mut tag.author,
            _ => bail!("AIFF text chunks have no key for {field:?} at {key:?}"),
        };
        *chunk = Some(value.clone());
        changed = true;
    }
    Ok(changed)
}

fn txxx(key: &str) -> &str {
    key.strip_prefix("TXXX:").unwrap_or(key)
}

/// `----:<mean>:<name>` as an MP4 freeform identifier. Every MP4 raw key in the
/// tables has this form.
fn freeform(key: &str) -> AtomIdent<'static> {
    let rest = key.strip_prefix("----:").unwrap_or(key);
    let (mean, name) = rest.split_once(':').unwrap_or(("", rest));
    AtomIdent::Freeform {
        mean: mean.to_owned().into(),
        name: name.to_owned().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use lofty::id3::v1::Id3v1Tag;
    use lofty::id3::v2::UniqueFileIdentifierFrame;
    use lofty::iff::aiff::Comment;
    use lofty::mp4::DataType;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::Accessor;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, atomic::AtomicBool};

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tyranny-tags-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn copy(&self, fixture: &str) -> PathBuf {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/formats")
                .join(fixture);
            let target = self.0.join(fixture);
            fs::copy(source, &target).unwrap();
            target
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn values() -> BTreeMap<TagField, String> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tag_roundtrip_values.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn setting(fields: &[(TagField, &str)]) -> TagEdit {
        TagEdit {
            set: fields
                .iter()
                .map(|(field, value)| (*field, Some((*value).to_owned())))
                .collect(),
            restore: Vec::new(),
        }
    }

    fn png() -> Picture {
        let data = base64::engine::general_purpose::STANDARD
            .decode(PNG_1X1)
            .unwrap();
        Picture::unchecked(data)
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build()
    }

    fn probe(path: &Path) -> crate::library_types::TrackMetadata {
        crate::metadata::probe(path, &Arc::new(AtomicBool::new(false))).unwrap()
    }

    fn read_flac(path: &Path) -> FlacFile {
        FlacFile::read_from(&mut fs::File::open(path).unwrap(), ParseOptions::new()).unwrap()
    }

    fn read_mpeg(path: &Path) -> MpegFile {
        MpegFile::read_from(&mut fs::File::open(path).unwrap(), ParseOptions::new()).unwrap()
    }

    const TITLE_ARTIST_ALBUM: [TagField; 3] = [TagField::Title, TagField::Artist, TagField::Album];

    /// `tone.mp3` with ID3v1 and APEv2 copies of the title, artist and album.
    fn mp3_with_stale_copies(scratch: &Scratch) -> PathBuf {
        let path = scratch.copy("tone.mp3");
        let mut id3v1 = Id3v1Tag::new();
        id3v1.set_title("Old Title".to_owned());
        id3v1.set_artist("Old Artist".to_owned());
        id3v1.set_album("Old Album".to_owned());
        id3v1.save_to_path(&path, WriteOptions::default()).unwrap();
        let mut ape = ApeTag::new();
        ape.set_title("Old Title".to_owned());
        ape.set_artist("Old Artist".to_owned());
        ape.set_album("Old Album".to_owned());
        ape.save_to_path(&path, WriteOptions::default()).unwrap();
        path
    }

    fn new_title_artist_album() -> TagEdit {
        setting(&[
            (TagField::Title, "New Title"),
            (TagField::Artist, "New Artist"),
            (TagField::Album, "New Album"),
        ])
    }

    #[test]
    fn writes_every_field_into_the_primary_tag_and_reads_it_back() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let values = values();
        let edit = TagEdit {
            set: values
                .iter()
                .map(|(field, value)| (*field, Some(value.clone())))
                .collect(),
            restore: Vec::new(),
        };
        edit_tags(&path, &edit).unwrap();
        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.format, WritableFormat::Flac);
        assert_eq!(tags.primary_kind, TagKind::VorbisComments);
        assert_eq!(tags.primary, values);
        assert!(tags.copies.is_empty(), "{:?}", tags.copies);
    }

    #[test]
    fn stale_id3v1_and_apev2_copies_are_removed_and_probe_reads_the_new_values() {
        let scratch = Scratch::new();
        let path = mp3_with_stale_copies(&scratch);
        let fields: BTreeSet<TagField> = TITLE_ARTIST_ALBUM.into_iter().collect();
        let before = read_tags(&path).unwrap();
        for kind in [TagKind::Id3v1, TagKind::Ape] {
            for (field, value) in
                TITLE_ARTIST_ALBUM
                    .iter()
                    .zip(["Old Title", "Old Artist", "Old Album"])
            {
                assert!(
                    before.copies.contains(&TagCopy {
                        tag: kind,
                        field: *field,
                        key: CopyKey::Field,
                        value: value.to_owned(),
                        read_only: false,
                    }),
                    "{kind:?} {field:?} missing from {:?}",
                    before.copies
                );
            }
        }
        let expected = before.copies_of(&fields);
        assert_eq!(expected.len(), 6, "{expected:?}");

        let removed = edit_tags(&path, &new_title_artist_album()).unwrap();
        assert_eq!(removed, expected);

        let metadata = probe(&path);
        assert_eq!(metadata.title, "New Title");
        assert_eq!(metadata.artist, "New Artist");
        assert_eq!(metadata.album, "New Album");
        let after = read_tags(&path).unwrap();
        assert!(after.copies_of(&fields).is_empty(), "{:?}", after.copies);
        let mpeg = read_mpeg(&path);
        assert!(mpeg.id3v1().is_none());
        assert!(mpeg.ape().is_none());
    }

    #[test]
    fn vorbis_release_id_alias_is_removed() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let mut comments = VorbisComments::new();
        comments.push("MUSICBRAINZ_RELEASEID".to_owned(), "old".to_owned());
        comments
            .save_to_path(&path, WriteOptions::default())
            .unwrap();
        let stale = TagCopy {
            tag: TagKind::VorbisComments,
            field: TagField::MusicbrainzReleaseId,
            key: CopyKey::Raw("MUSICBRAINZ_RELEASEID".to_owned()),
            value: "old".to_owned(),
            read_only: false,
        };
        assert!(read_tags(&path).unwrap().copies.contains(&stale));

        let new = &values()[&TagField::MusicbrainzReleaseId];
        let removed = edit_tags(&path, &setting(&[(TagField::MusicbrainzReleaseId, new)])).unwrap();
        assert!(removed.contains(&stale), "{removed:?}");
        assert_eq!(
            probe(&path).musicbrainz_release_id.as_deref(),
            Some(new.as_str())
        );
        let flac = read_flac(&path);
        let comments = flac.vorbis_comments().unwrap();
        assert_eq!(comments.get("MUSICBRAINZ_RELEASEID"), None);
        assert_eq!(comments.get("MUSICBRAINZ_ALBUMID"), Some(new.as_str()));
    }

    #[test]
    fn unmapped_items_survive_in_vorbis_and_ape() {
        let scratch = Scratch::new();
        let flac = scratch.copy("tone.flac");
        let mut comments = VorbisComments::new();
        comments.push("ASIN".to_owned(), "B000TEST".to_owned());
        comments
            .save_to_path(&flac, WriteOptions::default())
            .unwrap();
        let ape = scratch.copy("tone.ape");
        let mut tag = ApeTag::new();
        tag.insert(
            ApeItem::new("ASIN".to_owned(), ItemValue::Text("B000TEST".to_owned())).unwrap(),
        );
        tag.save_to_path(&ape, WriteOptions::default()).unwrap();

        for path in [&flac, &ape] {
            edit_tags(path, &setting(&[(TagField::Title, "New Title")])).unwrap();
            assert_eq!(
                read_tags(path).unwrap().primary[&TagField::Title],
                "New Title"
            );
        }
        assert_eq!(
            read_flac(&flac).vorbis_comments().unwrap().get("ASIN"),
            Some("B000TEST")
        );
        let ape_file =
            ApeFile::read_from(&mut fs::File::open(&ape).unwrap(), ParseOptions::new()).unwrap();
        let item = ape_file.ape().unwrap().get("ASIN").unwrap();
        assert_eq!(item.value().text(), Some("B000TEST"));
    }

    /// The identifiers of the file's ID3v2 `UFID` frames with this owner.
    fn ufids(path: &Path, owner: &str) -> Vec<String> {
        read_mpeg(path)
            .id3v2()
            .into_iter()
            .flatten()
            .filter_map(|frame| match frame {
                Frame::UniqueFileIdentifier(ufid) if ufid.owner == owner => {
                    Some(String::from_utf8(ufid.identifier.to_vec()).unwrap())
                }
                _ => None,
            })
            .collect()
    }

    fn txxx_recording_id(path: &Path) -> Option<String> {
        read_mpeg(path)
            .id3v2()?
            .get_user_text("MusicBrainz Track Id")
            .map(str::to_owned)
    }

    const MUSICBRAINZ_UFID: &str = "http://musicbrainz.org";

    #[test]
    fn id3_recording_id_is_written_as_txxx_and_ufid() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.mp3");
        let mut generic = Tag::new(TagType::Id3v2);
        assert!(generic.insert_text(ItemKey::MusicBrainzRecordingId, "old-recording".to_owned()));
        let mut id3 = Id3v2Tag::from(generic);
        id3.insert(Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
            "http://example.com",
            b"other-id".to_vec(),
        )));
        id3.save_to_path(&path, WriteOptions::default()).unwrap();
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["old-recording"]);
        let old_ufid = TagCopy {
            tag: TagKind::Id3v2,
            field: TagField::MusicbrainzRecordingId,
            key: CopyKey::Field,
            value: "old-recording".to_owned(),
            read_only: false,
        };
        let before = read_tags(&path).unwrap();
        assert_eq!(before.copies, std::slice::from_ref(&old_ufid));
        // Without a TXXX value, the current value is the UFID's.
        assert_eq!(
            before.current(TagField::MusicbrainzRecordingId),
            Some("old-recording")
        );

        let new = &values()[&TagField::MusicbrainzRecordingId];
        let removed =
            edit_tags(&path, &setting(&[(TagField::MusicbrainzRecordingId, new)])).unwrap();
        assert_eq!(removed, std::slice::from_ref(&old_ufid));
        assert_eq!(txxx_recording_id(&path).as_ref(), Some(new));
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), std::slice::from_ref(new));
        assert_eq!(ufids(&path, "http://example.com"), ["other-id"]);
        assert_eq!(
            probe(&path).musicbrainz_recording_id.as_deref(),
            Some(new.as_str())
        );
        // The UFID is the field's second home, not a copy.
        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.primary[&TagField::MusicbrainzRecordingId], *new);
        assert!(tags.copies.is_empty(), "{:?}", tags.copies);

        // Reverting removes both homes and puts the old UFID back.
        let revert = TagEdit {
            set: [(TagField::MusicbrainzRecordingId, None)]
                .into_iter()
                .collect(),
            restore: removed,
        };
        edit_tags(&path, &revert).unwrap();
        assert_eq!(txxx_recording_id(&path), None);
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["old-recording"]);
        assert_eq!(ufids(&path, "http://example.com"), ["other-id"]);
        let after = read_tags(&path).unwrap();
        assert_eq!(after.primary, before.primary);
        assert_eq!(after.copies, before.copies);
    }

    #[test]
    fn reverting_an_id3_recording_id_restores_both_homes() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.mp3");
        let recording = |value: &str| setting(&[(TagField::MusicbrainzRecordingId, value)]);
        let back_to = |value: &str, removed: Vec<TagCopy>| TagEdit {
            set: [(TagField::MusicbrainzRecordingId, Some(value.to_owned()))]
                .into_iter()
                .collect(),
            restore: removed,
        };

        // Both homes hold the same value: revert writes it to both again.
        edit_tags(&path, &recording("first")).unwrap();
        let removed = edit_tags(&path, &recording("second")).unwrap();
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["second"]);
        edit_tags(&path, &back_to("first", removed)).unwrap();
        assert_eq!(txxx_recording_id(&path).as_deref(), Some("first"));
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["first"]);
        assert!(read_tags(&path).unwrap().copies.is_empty());

        // The UFID differs from the TXXX: it is a copy, and revert restores it as it was.
        let mut id3 = read_mpeg(&path).id3v2().unwrap().clone();
        id3.insert(Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
            MUSICBRAINZ_UFID,
            b"other".to_vec(),
        )));
        id3.save_to_path(&path, WriteOptions::default()).unwrap();
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["other"]);
        let before = read_tags(&path).unwrap();
        assert_eq!(before.primary[&TagField::MusicbrainzRecordingId], "first");
        let removed = edit_tags(&path, &recording("second")).unwrap();
        assert_eq!(
            removed,
            [TagCopy {
                tag: TagKind::Id3v2,
                field: TagField::MusicbrainzRecordingId,
                key: CopyKey::Field,
                value: "other".to_owned(),
                read_only: false,
            }]
        );
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["second"]);
        edit_tags(&path, &back_to("first", removed)).unwrap();
        assert_eq!(txxx_recording_id(&path).as_deref(), Some("first"));
        assert_eq!(ufids(&path, MUSICBRAINZ_UFID), ["other"]);
        let after = read_tags(&path).unwrap();
        assert_eq!(after.primary, before.primary);
        assert_eq!(after.copies, before.copies);
    }

    #[test]
    fn restore_puts_removed_copies_back() {
        let scratch = Scratch::new();
        let path = mp3_with_stale_copies(&scratch);
        let fields: BTreeSet<TagField> = TITLE_ARTIST_ALBUM.into_iter().collect();
        let before = read_tags(&path).unwrap();
        let removed = edit_tags(&path, &new_title_artist_album()).unwrap();
        assert!(!removed.is_empty());

        let revert = TagEdit {
            set: fields
                .iter()
                .map(|field| (*field, before.primary.get(field).cloned()))
                .collect(),
            restore: removed,
        };
        edit_tags(&path, &revert).unwrap();
        let after = read_tags(&path).unwrap();
        assert_eq!(after.copies_of(&fields), before.copies_of(&fields));
        for field in &fields {
            assert_eq!(
                after.primary.get(field),
                before.primary.get(field),
                "{field:?}"
            );
        }
        let mpeg = read_mpeg(&path);
        assert_eq!(mpeg.id3v1().unwrap().title().as_deref(), Some("Old Title"));
        assert_eq!(mpeg.ape().unwrap().album().as_deref(), Some("Old Album"));
    }

    #[test]
    fn untouched_items_and_pictures_survive() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let mut flac = read_flac(&path);
        let mut comments = VorbisComments::new();
        comments.set_genre("Keep Genre".to_owned());
        comments.set_comment("Keep Comment".to_owned());
        // lofty's merge would make this the vendor string and drop the comment.
        comments.push("ENCODER".to_owned(), "Keep Encoder".to_owned());
        flac.set_vorbis_comments(comments);
        flac.insert_picture(png(), None).unwrap();
        flac.save_to_path(&path, WriteOptions::default()).unwrap();

        edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).unwrap();
        let flac = read_flac(&path);
        let comments = flac.vorbis_comments().unwrap();
        assert_eq!(comments.title().as_deref(), Some("New Title"));
        assert_eq!(comments.genre().as_deref(), Some("Keep Genre"));
        assert_eq!(comments.comment().as_deref(), Some("Keep Comment"));
        assert_eq!(comments.get("ENCODER"), Some("Keep Encoder"));
        let pictures: Vec<_> = flac
            .pictures()
            .iter()
            .chain(comments.pictures())
            .map(|(picture, _)| picture)
            .collect();
        assert_eq!(pictures.len(), 1, "{pictures:?}");
        assert_eq!(pictures[0].pic_type(), PictureType::CoverFront);
        assert_eq!(pictures[0].data(), png().data());
    }

    #[test]
    fn read_only_id3_in_flac_is_flagged() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let mut id3 = Id3v2Tag::new();
        id3.set_title("Old Title".to_owned());
        let mut bytes = Vec::new();
        id3.dump_to(&mut bytes, WriteOptions::default().use_id3v23(true))
            .unwrap();
        assert_eq!(bytes[..4], *b"ID3\x03");
        bytes.extend(fs::read(&path).unwrap());
        fs::write(&path, &bytes).unwrap();

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.format, WritableFormat::Flac);
        // The v2.3 tag is not the primary tag, so edits need not keep v2.3.
        assert!(!tags.id3v23);
        let title: BTreeSet<TagField> = [TagField::Title].into_iter().collect();
        assert_eq!(tags.read_only_reason(&title), Some(READ_ONLY_REASON));
        let album: BTreeSet<TagField> = [TagField::Album].into_iter().collect();
        assert_eq!(tags.read_only_reason(&album), None);
        assert!(tags.copies.contains(&TagCopy {
            tag: TagKind::Id3v2,
            field: TagField::Title,
            key: CopyKey::Field,
            value: "Old Title".to_owned(),
            read_only: true,
        }));
        let error = edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).unwrap_err();
        assert!(error.downcast_ref::<ReadOnlyTag>().is_some(), "{error:#}");
        assert_eq!(error.root_cause().to_string(), READ_ONLY_REASON);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        // A field the read-only tag lacks is still writable.
        edit_tags(&path, &setting(&[(TagField::Album, "New Album")])).unwrap();
    }

    /// `tone.mp3` is ID3v2.4; this copy is rewritten as ID3v2.3.
    fn mp3_with_id3v23(scratch: &Scratch) -> PathBuf {
        let path = scratch.copy("tone.mp3");
        assert_eq!(fs::read(&path).unwrap()[3], 4);
        let mut id3 = Id3v2Tag::new();
        id3.set_artist("Old Artist".to_owned());
        id3.save_to_path(&path, WriteOptions::default().use_id3v23(true))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap()[..4], *b"ID3\x03");
        path
    }

    #[test]
    fn id3v23_files_stay_v23() {
        let scratch = Scratch::new();
        let path = mp3_with_id3v23(&scratch);
        edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).unwrap();
        assert_eq!(fs::read(&path).unwrap()[..4], *b"ID3\x03");
        let tags = read_tags(&path).unwrap();
        assert!(tags.id3v23);
        assert_eq!(tags.primary[&TagField::Title], "New Title");
        assert_eq!(tags.primary[&TagField::Artist], "Old Artist");
    }

    #[test]
    fn id3v23_round_trip_follows_its_deny_list() {
        let scratch = Scratch::new();
        let path = mp3_with_id3v23(&scratch);
        let values = values();
        let edit = TagEdit {
            set: values
                .iter()
                .map(|(field, value)| (*field, Some(value.clone())))
                .collect(),
            restore: Vec::new(),
        };
        edit_tags(&path, &edit).unwrap();
        let tags = read_tags(&path).unwrap();
        assert!(tags.id3v23);
        assert_eq!(unwritable_fields_in(&tags), ID3V23_UNWRITABLE);
        for (field, value) in &values {
            let read = tags.primary.get(field);
            if ID3V23_UNWRITABLE.contains(field) {
                assert_ne!(
                    read,
                    Some(value),
                    "{field:?} round-trips, so drop it from the list"
                );
            } else {
                assert_eq!(read, Some(value), "{field:?}");
            }
        }
        assert_eq!(tags.primary[&TagField::OriginalDate], "1999");
    }

    fn read_ape(path: &Path) -> Option<ApeTag> {
        let reader = &mut fs::File::open(path).unwrap();
        if path.extension().is_some_and(|extension| extension == "mp3") {
            MpegFile::read_from(reader, ParseOptions::new())
                .unwrap()
                .remove_ape()
        } else {
            ApeFile::read_from(reader, ParseOptions::new())
                .unwrap()
                .remove_ape()
        }
    }

    fn ape_text(path: &Path, key: &str) -> Option<String> {
        read_ape(path)?
            .get(key)
            .and_then(|item| item.value().text())
            .map(str::to_owned)
    }

    fn with_ape_items(path: &Path, items: &[(&str, &str)]) {
        let mut tag = ApeTag::new();
        for (key, value) in items {
            tag.insert(
                ApeItem::new((*key).to_owned(), ItemValue::Text((*value).to_owned())).unwrap(),
            );
        }
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    fn fields(fields: &[TagField]) -> BTreeSet<TagField> {
        fields.iter().copied().collect()
    }

    const TRACK: [TagField; 2] = [TagField::TrackNumber, TagField::TrackTotal];

    #[test]
    fn ape_track_pair_in_mp3_leaves_no_stale_copies() {
        let scratch = Scratch::new();
        let track = fields(&TRACK);

        // Both halves of the pair.
        let both = scratch.copy("tone.mp3");
        with_ape_items(&both, &[("Track", "3/12")]);
        let before = read_tags(&both).unwrap();
        assert_eq!(before.copies_of(&track).len(), 2, "{:?}", before.copies);
        edit_tags(
            &both,
            &setting(&[(TagField::TrackNumber, "5"), (TagField::TrackTotal, "20")]),
        )
        .unwrap();
        assert!(read_tags(&both).unwrap().copies_of(&track).is_empty());
        assert_eq!(ape_text(&both, "Track"), None);
        let metadata = probe(&both);
        assert_eq!(
            (metadata.track_number, metadata.track_total),
            (Some(5), Some(20))
        );
        // Then the number alone.
        edit_tags(&both, &setting(&[(TagField::TrackNumber, "6")])).unwrap();
        let tags = read_tags(&both).unwrap();
        assert!(tags.copies_of(&track).is_empty(), "{:?}", tags.copies);
        let metadata = probe(&both);
        assert_eq!(
            (metadata.track_number, metadata.track_total),
            (Some(6), Some(20))
        );

        // The number alone: the APE item goes whole, and its total is journalled.
        let alone = scratch.copy("tone.mp3");
        fs::rename(&alone, scratch.0.join("alone.mp3")).unwrap();
        let alone = scratch.0.join("alone.mp3");
        with_ape_items(&alone, &[("Track", "3/12"), ("Title", "Keep")]);
        let before = read_tags(&alone).unwrap();
        let number = fields(&[TagField::TrackNumber]);
        let removed = edit_tags(&alone, &setting(&[(TagField::TrackNumber, "5")])).unwrap();
        let copy = |field, value: &str| TagCopy {
            tag: TagKind::Ape,
            field,
            key: CopyKey::Field,
            value: value.to_owned(),
            read_only: false,
        };
        assert_eq!(
            removed,
            [
                copy(TagField::TrackNumber, "3"),
                copy(TagField::TrackTotal, "12")
            ]
        );
        // The prediction matches what the write dropped.
        assert_eq!(
            before.partners_dropped_with(&number),
            [copy(TagField::TrackTotal, "12")]
        );
        assert!(before.partners_dropped_with(&track).is_empty());
        assert_eq!(ape_text(&alone, "Track"), None);
        assert_eq!(ape_text(&alone, "Title").as_deref(), Some("Keep"));
        assert!(read_tags(&alone).unwrap().copies_of(&track).is_empty());
        assert_eq!(probe(&alone).track_number, Some(5));

        // Reverting puts the whole pair back.
        let revert = TagEdit {
            set: [(
                TagField::TrackNumber,
                before.primary.get(&TagField::TrackNumber).cloned(),
            )]
            .into_iter()
            .collect(),
            restore: removed,
        };
        edit_tags(&alone, &revert).unwrap();
        assert_eq!(ape_text(&alone, "Track").as_deref(), Some("3/12"));
        assert_eq!(read_tags(&alone).unwrap().copies, before.copies);
    }

    #[test]
    fn removing_one_half_of_a_primary_pair_leaves_no_zero() {
        let scratch = Scratch::new();
        // APE primary: "/12" keeps the total without a number.
        let ape = scratch.copy("tone.ape");
        with_ape_items(&ape, &[("Track", "3/12")]);
        let removed = edit_tags(
            &ape,
            &TagEdit {
                set: [(TagField::TrackNumber, None)].into_iter().collect(),
                restore: Vec::new(),
            },
        )
        .unwrap();
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(ape_text(&ape, "Track").as_deref(), Some("/12"));
        let tags = read_tags(&ape).unwrap();
        assert_eq!(tags.primary.get(&TagField::TrackNumber), None);
        assert_eq!(tags.primary[&TagField::TrackTotal], "12");
        let metadata = probe(&ape);
        assert_eq!(
            (metadata.track_number, metadata.track_total),
            (None, Some(12))
        );
        with_ape_items(&ape, &[("Track", "3/12")]);
        edit_tags(
            &ape,
            &TagEdit {
                set: [(TagField::TrackTotal, None)].into_iter().collect(),
                restore: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(ape_text(&ape, "Track").as_deref(), Some("3"));

        // ID3v2 primary: a total without a number would be written "0/12".
        let id3_track = |path: &Path| {
            read_mpeg(path)
                .id3v2()
                .and_then(|tag| tag.get_text(&FrameId::Valid(Cow::Borrowed("TRCK"))))
                .map(str::to_owned)
        };
        let mp3 = scratch.copy("tone.mp3");
        let write_trck = |path: &Path| {
            let mut id3 = Id3v2Tag::new();
            id3.set_track(3);
            id3.set_track_total(12);
            id3.save_to_path(path, WriteOptions::default()).unwrap();
        };
        write_trck(&mp3);
        assert_eq!(id3_track(&mp3).as_deref(), Some("3/12"));
        edit_tags(
            &mp3,
            &TagEdit {
                set: [(TagField::TrackTotal, None)].into_iter().collect(),
                restore: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(id3_track(&mp3).as_deref(), Some("3"));
        write_trck(&mp3);
        let removed = edit_tags(
            &mp3,
            &TagEdit {
                set: [(TagField::TrackNumber, None)].into_iter().collect(),
                restore: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(id3_track(&mp3), None);
        assert_eq!(
            removed,
            [TagCopy {
                tag: TagKind::Id3v2,
                field: TagField::TrackTotal,
                key: CopyKey::Field,
                value: "12".to_owned(),
                read_only: false,
            }]
        );
        let tags = read_tags(&mp3).unwrap();
        assert!(tags.copies_of(&fields(&TRACK)).is_empty());
        assert!(!tags.primary.contains_key(&TagField::TrackNumber));
        assert!(!tags.primary.contains_key(&TagField::TrackTotal));
        // A total alone is refused, and nothing is written.
        let bytes = fs::read(&mp3).unwrap();
        assert!(edit_tags(&mp3, &setting(&[(TagField::TrackTotal, "12")])).is_err());
        assert_eq!(fs::read(&mp3).unwrap(), bytes);
    }

    #[test]
    fn non_numeric_track_numbers_survive_and_are_copies() {
        let scratch = Scratch::new();
        let ape = scratch.copy("tone.ape");
        with_ape_items(&ape, &[("Track", "A1")]);
        assert_eq!(
            read_tags(&ape).unwrap().primary[&TagField::TrackNumber],
            "A1"
        );
        edit_tags(&ape, &setting(&[(TagField::Title, "New Title")])).unwrap();
        assert_eq!(ape_text(&ape, "Track").as_deref(), Some("A1"));

        // lofty leaves an ID3v2 TRCK it can't parse out of its generic tag.
        let mp3 = scratch.copy("tone.mp3");
        let mut id3 = Id3v2Tag::new();
        id3.insert(Frame::Text(TextInformationFrame::new(
            FrameId::Valid(Cow::Borrowed("TRCK")),
            TextEncoding::UTF8,
            "A1",
        )));
        id3.save_to_path(&mp3, WriteOptions::default()).unwrap();
        let unparsed = TagCopy {
            tag: TagKind::Id3v2,
            field: TagField::TrackNumber,
            key: CopyKey::Raw("TRCK".to_owned()),
            value: "A1".to_owned(),
            read_only: false,
        };
        assert_eq!(
            read_tags(&mp3).unwrap().copies,
            std::slice::from_ref(&unparsed)
        );
        edit_tags(&mp3, &setting(&[(TagField::Title, "New Title")])).unwrap();
        assert_eq!(
            read_tags(&mp3).unwrap().copies,
            std::slice::from_ref(&unparsed)
        );
        let removed = edit_tags(&mp3, &setting(&[(TagField::TrackNumber, "1")])).unwrap();
        assert_eq!(removed, std::slice::from_ref(&unparsed));
        let tags = read_tags(&mp3).unwrap();
        assert_eq!(tags.primary[&TagField::TrackNumber], "1");
        assert!(tags.copies.is_empty(), "{:?}", tags.copies);
        let trcks = read_mpeg(&mp3)
            .id3v2()
            .unwrap()
            .into_iter()
            .filter(|frame| frame.id_str() == "TRCK")
            .count();
        assert_eq!(trcks, 1);
        edit_tags(
            &mp3,
            &TagEdit {
                set: [(TagField::TrackNumber, None)].into_iter().collect(),
                restore: removed,
            },
        )
        .unwrap();
        assert_eq!(read_tags(&mp3).unwrap().copies, [unparsed]);
    }

    #[test]
    fn riff_info_keeps_unmapped_items() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.wav");
        let mut info = RiffInfoList::new();
        info.insert("INAM".to_owned(), "Old Title".to_owned());
        info.insert("IENG".to_owned(), "Engineer".to_owned());
        info.insert("IKEY".to_owned(), "Keywords".to_owned());
        info.save_to_path(&path, WriteOptions::default()).unwrap();
        let removed = edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).unwrap();
        assert_eq!(
            removed,
            [TagCopy {
                tag: TagKind::RiffInfo,
                field: TagField::Title,
                key: CopyKey::Field,
                value: "Old Title".to_owned(),
                read_only: false,
            }]
        );
        let wav =
            WavFile::read_from(&mut fs::File::open(&path).unwrap(), ParseOptions::new()).unwrap();
        let info = wav.riff_info().unwrap();
        assert_eq!(info.get("INAM"), None);
        assert_eq!(info.get("IENG"), Some("Engineer"));
        assert_eq!(info.get("IKEY"), Some("Keywords"));
        assert_eq!(
            read_tags(&path).unwrap().primary[&TagField::Title],
            "New Title"
        );
        assert_eq!(probe(&path).title, "New Title");
    }

    #[test]
    fn id3v23_chunks_in_wav_and_aiff_stay_v23() {
        let scratch = Scratch::new();
        for fixture in ["tone.wav", "tone.aiff"] {
            let path = scratch.copy(fixture);
            let mut id3 = Id3v2Tag::new();
            id3.set_artist("Old Artist".to_owned());
            id3.save_to_path(&path, WriteOptions::default().use_id3v23(true))
                .unwrap();
            let tags = read_tags(&path).unwrap();
            assert!(tags.id3v23, "{fixture}");
            assert_eq!(unwritable_fields_in(&tags), ID3V23_UNWRITABLE, "{fixture}");
            edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).unwrap();
            let file = &mut fs::File::open(&path).unwrap();
            let version = if fixture.ends_with("wav") {
                WavFile::read_from(file, ParseOptions::new())
                    .unwrap()
                    .id3v2()
                    .unwrap()
                    .original_version()
            } else {
                AiffFile::read_from(file, ParseOptions::new())
                    .unwrap()
                    .id3v2()
                    .unwrap()
                    .original_version()
            };
            assert_eq!(version, Id3v2Version::V3, "{fixture}");
            let tags = read_tags(&path).unwrap();
            assert!(tags.id3v23, "{fixture}");
            assert_eq!(tags.primary[&TagField::Title], "New Title", "{fixture}");
            assert_eq!(tags.primary[&TagField::Artist], "Old Artist", "{fixture}");
        }
    }

    #[test]
    fn aiff_comments_keep_their_timestamps_and_markers() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.aiff");
        let comments = vec![Comment {
            timestamp: 1_234,
            marker_id: 7,
            text: "Keep Comment".to_owned(),
        }];
        let mut chunks = AiffTextChunks::new();
        chunks.name = Some("Old Title".to_owned());
        chunks.comments = Some(comments.clone());
        chunks.save_to_path(&path, WriteOptions::default()).unwrap();
        let read = |path: &Path| {
            AiffFile::read_from(&mut fs::File::open(path).unwrap(), ParseOptions::new())
                .unwrap()
                .remove_text_chunks()
                .unwrap()
        };
        assert_eq!(read(&path).comments, Some(comments.clone()));
        let removed = edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).unwrap();
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert_eq!(removed[0].tag, TagKind::AiffText);
        let chunks = read(&path);
        assert_eq!(chunks.name, None);
        assert_eq!(chunks.comments, Some(comments));
        assert_eq!(chunks.annotations, None);
    }

    #[test]
    fn binary_data_at_a_removed_key_stays() {
        let scratch = Scratch::new();
        let new = &values()[&TagField::MusicbrainzReleaseId];
        let alias = AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: "MusicBrainz Release Id".into(),
        };
        let binary = AtomData::Unknown {
            code: DataType::Reserved,
            data: b"binary".to_vec(),
        };
        let m4a = scratch.copy("tone.m4a");
        let mut ilst = Ilst::new();
        ilst.insert(
            Atom::from_collection(
                alias.clone(),
                vec![AtomData::UTF8("old".to_owned()), binary.clone()],
            )
            .unwrap(),
        );
        ilst.save_to_path(&m4a, WriteOptions::default()).unwrap();
        let removed = edit_tags(&m4a, &setting(&[(TagField::MusicbrainzReleaseId, new)])).unwrap();
        assert_eq!(
            removed,
            [TagCopy {
                tag: TagKind::Mp4Ilst,
                field: TagField::MusicbrainzReleaseId,
                key: CopyKey::Raw("----:com.apple.iTunes:MusicBrainz Release Id".to_owned()),
                value: "old".to_owned(),
                read_only: false,
            }]
        );
        let mp4 =
            Mp4File::read_from(&mut fs::File::open(&m4a).unwrap(), ParseOptions::new()).unwrap();
        let data: Vec<&AtomData> = mp4.ilst().unwrap().get(&alias).unwrap().data().collect();
        assert_eq!(data, [&binary]);
        assert_eq!(
            read_tags(&m4a).unwrap().primary[&TagField::MusicbrainzReleaseId],
            *new
        );

        let ape = scratch.copy("tone.ape");
        let mut tag = ApeTag::new();
        tag.insert(
            ApeItem::new(
                "MUSICBRAINZ_RELEASEID".to_owned(),
                ItemValue::Binary(b"binary".to_vec()),
            )
            .unwrap(),
        );
        tag.save_to_path(&ape, WriteOptions::default()).unwrap();
        let removed = edit_tags(&ape, &setting(&[(TagField::MusicbrainzReleaseId, new)])).unwrap();
        assert!(removed.is_empty(), "{removed:?}");
        let item = read_ape(&ape).unwrap();
        let item = item.get("MUSICBRAINZ_RELEASEID").unwrap();
        assert_eq!(item.value(), &ItemValue::Binary(b"binary".to_vec()));
    }

    #[test]
    fn edits_go_through_the_handle_not_the_path() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let moved = scratch.0.join("moved.flac");
        fs::rename(&path, &moved).unwrap();
        assert!(!path.exists());
        edit_tags_file(&mut file, &setting(&[(TagField::Title, "New Title")])).unwrap();
        assert!(!path.exists());
        assert_eq!(
            read_tags_file(&mut file).unwrap().primary[&TagField::Title],
            "New Title"
        );
        assert_eq!(
            read_tags(&moved).unwrap().primary[&TagField::Title],
            "New Title"
        );
    }
}
