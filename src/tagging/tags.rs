//! Tag fields in audio files, read and written with lofty.
//!
//! Each field has one primary home: its key in the file's primary tag. Every
//! other value of the field is a copy: an alias key in the primary tag, an extra
//! value of the primary key, or the field in another tag. An edit removes every
//! copy of each field it sets, so FFmpeg (which merges all tags and accepts
//! aliases) reads the new value.
//!
//! lofty's generic `Tag` drops unmapped Vorbis and APE items. So Vorbis comments
//! and APE tags are read from the concrete file types and edited in place. Their
//! mapped items go through `SplitTag`/`MergeTag`. Other tags use the generic `Tag`.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use lofty::ape::{ApeFile, ApeItem, ApeTag};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::id3::v2::{Frame, Id3v2Tag};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
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
    /// `----:<mean>:<name>` in MP4, the item key in Vorbis and APE.
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

/// The fields of one file, as lofty reads them.
#[derive(Clone, Debug)]
pub struct FileTags {
    pub format: WritableFormat,
    pub primary_kind: TagKind,
    /// The file starts with an ID3v2.3 tag, and edits keep it v2.3.
    pub id3v23: bool,
    /// Each field's value at its primary key in the primary tag.
    pub primary: BTreeMap<TagField, String>,
    /// Every other value of a field, in lofty's tag order.
    pub copies: Vec<TagCopy>,
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
}

/// An in-place change to one file's tags.
#[derive(Clone, Debug, Default)]
pub struct TagEdit {
    /// Each field is first removed everywhere; `Some` then writes it to its
    /// primary home.
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

/// The primary key is a raw key, not the field's ItemKey; the ItemKey copy is then an alias.
const PRIMARY_RAW: &[(TagKind, TagField, &str)] = &[(
    TagKind::Id3v2,
    TagField::MusicbrainzRecordingId,
    "TXXX:MusicBrainz Track Id",
)];

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
    let mut head = Vec::with_capacity(36);
    File::open(path)?.take(36).read_to_end(&mut head)?;
    let file_type = if head.starts_with(b"ID3") {
        Probe::new(BufReader::new(File::open(path)?))
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

pub fn read_tags(path: &Path) -> Result<FileTags> {
    let parsed =
        Parsed::read(path).with_context(|| format!("reading tags of {}", path.display()))?;
    parsed.file_tags()
}

/// Edits the file in place. Returns the copies it removed.
pub fn edit_tags(path: &Path, edit: &TagEdit) -> Result<Vec<TagCopy>> {
    let context = || format!("writing tags to {}", path.display());
    let mut parsed = Parsed::read(path).with_context(context)?;
    let removed = parsed.apply(edit).with_context(context)?;
    parsed.save(path).with_context(context)?;
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
    /// ID3v2, ID3v1, MP4 ilst, RIFF INFO and AIFF text.
    Generic(Tag),
    Vorbis(VorbisComments),
    Ape(ApeTag),
}

impl Parsed {
    fn read(path: &Path) -> Result<Self> {
        let format = writable_format(path)?
            .ok_or_else(|| anyhow!("Tyranny can't write tags to this format"))?;
        let tagged = Probe::new(BufReader::new(File::open(path)?))
            .guess_file_type()?
            .read()?;
        let file_type = tagged.file_type();
        ensure!(
            format_of(file_type) == Some(format),
            "lofty read {file_type:?}, not {format:?}"
        );
        let (mut vorbis, mut ape) = concrete_tags(path, file_type)?;
        let mut slots = Vec::new();
        for tag in tagged.tags() {
            let kind = TagKind::of(tag.tag_type())?;
            let body = match kind {
                TagKind::VorbisComments => Body::Vorbis(
                    vorbis
                        .take()
                        .context("lofty listed Vorbis comments that the FLAC/Ogg reader lacks")?,
                ),
                TagKind::Ape => Body::Ape(
                    ape.take()
                        .context("lofty listed an APE tag that the concrete reader lacks")?,
                ),
                _ => Body::Generic(tag.clone()),
            };
            slots.push(Slot::new(file_type, kind, body));
        }
        if let Some(comments) = vorbis {
            slots.push(Slot::new(
                file_type,
                TagKind::VorbisComments,
                Body::Vorbis(comments),
            ));
        }
        if let Some(tag) = ape {
            slots.push(Slot::new(file_type, TagKind::Ape, Body::Ape(tag)));
        }
        let mut head = Vec::with_capacity(4);
        File::open(path)?.take(4).read_to_end(&mut head)?;
        Ok(Self {
            format,
            file_type,
            id3v23: head == b"ID3\x03",
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
        for slot in &self.slots {
            for (field, mut entries) in slot.entries() {
                if slot.kind == primary_kind {
                    let key = primary_key(slot.kind, field);
                    if let Some(position) = entries.iter().position(|(k, _)| *k == key) {
                        primary.insert(field, entries.remove(position).1);
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
        })
    }

    /// Applies the edit in memory. Returns the copies it removes.
    fn apply(&mut self, edit: &TagEdit) -> Result<Vec<TagCopy>> {
        let fields: BTreeSet<TagField> = edit.set.keys().copied().collect();
        let removed = self.file_tags()?.copies_of(&fields);
        if let Some(copy) = removed
            .iter()
            .chain(&edit.restore)
            .find(|copy| copy.read_only)
        {
            bail!(
                "Has an old ID3 tag Tyranny can't edit ({:?} holds {:?})",
                copy.tag,
                copy.field
            );
        }
        for slot in self.slots.iter_mut().filter(|slot| !slot.read_only) {
            for field in &fields {
                slot.remove(*field)?;
            }
        }
        let primary_kind = self.primary_kind()?;
        for (field, value) in &edit.set {
            if let Some(value) = value {
                let key = primary_key(primary_kind, *field);
                self.slot(primary_kind)?.push(*field, &key, value)?;
            }
        }
        for copy in &edit.restore {
            self.slot(copy.tag)?
                .push(copy.field, &copy.key, &copy.value)?;
        }
        Ok(removed)
    }

    /// The writable slot of this kind, created when absent.
    fn slot(&mut self, kind: TagKind) -> Result<&mut Slot> {
        if !self.file_type.tag_support(kind.tag_type()).is_writable() {
            bail!("lofty can't write {kind:?} tags to {:?} files", self.format);
        }
        let position = match self.slots.iter().position(|slot| slot.kind == kind) {
            Some(position) => position,
            None => {
                let body = match kind {
                    TagKind::VorbisComments => Body::Vorbis(VorbisComments::new()),
                    TagKind::Ape => Body::Ape(ApeTag::new()),
                    _ => Body::Generic(Tag::new(kind.tag_type())),
                };
                self.slots.push(Slot::new(self.file_type, kind, body));
                self.slots.len() - 1
            }
        };
        Ok(&mut self.slots[position])
    }

    fn save(&self, path: &Path) -> Result<()> {
        let mut options = WriteOptions::default();
        if self.id3v23 {
            options = options.use_id3v23(true);
        }
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        for slot in self.slots.iter().filter(|slot| slot.modified) {
            file.rewind()?;
            match &slot.body {
                Body::Generic(tag) => tag.save_to(&mut file, options),
                Body::Vorbis(comments) => comments.save_to(&mut file, options),
                Body::Ape(tag) => tag.save_to(&mut file, options),
            }
            .with_context(|| format!("saving the {:?} tag", slot.kind))?;
        }
        Ok(())
    }
}

/// Vorbis comments and APE tags, read through the concrete file types so that
/// unmapped items survive. FLAC pictures move into the Vorbis comments, because
/// saving them rewrites every PICTURE block from the tag's pictures.
fn concrete_tags(
    path: &Path,
    file_type: FileType,
) -> Result<(Option<VorbisComments>, Option<ApeTag>)> {
    let reader = &mut BufReader::new(File::open(path)?);
    let options = ParseOptions::new();
    Ok(match file_type {
        FileType::Flac => {
            let mut flac = FlacFile::read_from(reader, options)?;
            let pictures = flac.remove_pictures();
            let mut comments = flac.remove_vorbis_comments();
            if !pictures.is_empty() {
                let comments = comments.get_or_insert_with(VorbisComments::new);
                for (picture, information) in pictures {
                    comments.insert_picture(picture, Some(information))?;
                }
            }
            (comments, None)
        }
        FileType::Vorbis => (
            Some(VorbisFile::read_from(reader, options)?.remove_vorbis_comments()),
            None,
        ),
        FileType::Opus => (
            Some(OpusFile::read_from(reader, options)?.remove_vorbis_comments()),
            None,
        ),
        FileType::Speex => (
            Some(SpeexFile::read_from(reader, options)?.remove_vorbis_comments()),
            None,
        ),
        FileType::Ape => (None, ApeFile::read_from(reader, options)?.remove_ape()),
        FileType::Mpc => (None, MpcFile::read_from(reader, options)?.remove_ape()),
        FileType::WavPack => (None, WavPackFile::read_from(reader, options)?.remove_ape()),
        FileType::Mpeg => (None, MpegFile::read_from(reader, options)?.remove_ape()),
        _ => (None, None),
    })
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

    /// Every value of every field in this tag, in `TagField::ALL` order. Within a
    /// field: the ItemKey values, then `Year` (for `Date`), then the raw keys.
    fn entries(&self) -> Vec<(TagField, Vec<(CopyKey, String)>)> {
        let items = self.body.items();
        let raw = self.body.raw_values(self.kind);
        TagField::ALL
            .into_iter()
            .map(|field| {
                let mut entries: Vec<(CopyKey, String)> = items
                    .get_strings(item_key(field))
                    .map(|value| (CopyKey::Field, value.to_owned()))
                    .collect();
                if field == TagField::Date {
                    entries.extend(
                        items
                            .get_strings(ItemKey::Year)
                            .map(|value| (CopyKey::Year, value.to_owned())),
                    );
                }
                for key in raw_keys(self.kind, field) {
                    for value in raw.get(key).into_iter().flatten() {
                        entries.push((CopyKey::Raw(key.to_owned()), value.clone()));
                    }
                }
                (field, entries)
            })
            .filter(|(_, entries)| !entries.is_empty())
            .collect()
    }

    /// Removes every value of the field: its ItemKey, `Year` for `Date`, and its raw keys.
    fn remove(&mut self, field: TagField) -> Result<()> {
        let removed_items = self.body.edit_items(|items| {
            let before = items.item_count();
            items.remove_key(item_key(field));
            if field == TagField::Date {
                items.remove_key(ItemKey::Year);
            }
            items.item_count() != before
        })?;
        let mut removed_raw = false;
        for key in raw_keys(self.kind, field) {
            removed_raw |= self.body.remove_raw(self.kind, key)?;
        }
        self.modified |= removed_items || removed_raw;
        Ok(())
    }

    fn push(&mut self, field: TagField, key: &CopyKey, value: &str) -> Result<()> {
        let kind = self.kind;
        match key {
            CopyKey::Field | CopyKey::Year => {
                let item_key = match key {
                    CopyKey::Year => ItemKey::Year,
                    _ => item_key(field),
                };
                let item = TagItem::new(item_key, ItemValue::Text(value.to_owned()));
                if !self.body.edit_items(|items| items.push(item))? {
                    bail!("lofty can't store {item_key:?} in a {kind:?} tag");
                }
            }
            CopyKey::Raw(raw) => self.body.push_raw(kind, raw, value)?,
        }
        self.modified = true;
        Ok(())
    }
}

impl Body {
    /// The mapped items, as a generic `Tag`.
    fn items(&self) -> std::borrow::Cow<'_, Tag> {
        use std::borrow::Cow;
        match self {
            Body::Generic(tag) => Cow::Borrowed(tag),
            Body::Vorbis(comments) => Cow::Owned(comments.clone().split_tag().1),
            Body::Ape(tag) => Cow::Owned(tag.clone().split_tag().1),
        }
    }

    /// Edits the mapped items through a generic `Tag`. Unmapped Vorbis and APE
    /// items stay in the split remainder.
    fn edit_items<R>(&mut self, edit: impl FnOnce(&mut Tag) -> R) -> Result<R> {
        Ok(match self {
            Body::Generic(tag) => edit(tag),
            Body::Vorbis(comments) => {
                // Merging takes the vendor from an `ENCODER` comment when there
                // is one, and drops the comment. Pictures merge back only when
                // lofty can re-read their information. Keep all three aside.
                let vendor = comments.vendor().to_owned();
                let encoders: Vec<String> = comments.remove("ENCODER").collect();
                let pictures = comments.remove_pictures();
                let (rest, mut items) = std::mem::take(comments).split_tag();
                let result = edit(&mut items);
                *comments = rest.merge_tag(items);
                comments.set_vendor(vendor);
                for encoder in encoders {
                    comments.push("ENCODER".to_owned(), encoder);
                }
                for (picture, information) in pictures {
                    comments.insert_picture(picture, Some(information))?;
                }
                result
            }
            Body::Ape(tag) => {
                let (rest, mut items) = std::mem::take(tag).split_tag();
                let result = edit(&mut items);
                *tag = rest.merge_tag(items);
                result
            }
        })
    }

    /// The values at each raw key this tag kind has.
    fn raw_values(&self, kind: TagKind) -> BTreeMap<&'static str, Vec<String>> {
        let view = match self {
            Body::Vorbis(comments) => RawView::Vorbis(comments),
            Body::Ape(tag) => RawView::Ape(tag),
            Body::Generic(tag) if kind == TagKind::Id3v2 => {
                RawView::Id3v2(Id3v2Tag::from(tag.clone()))
            }
            Body::Generic(tag) if kind == TagKind::Mp4Ilst => {
                RawView::Ilst(Ilst::from(tag.clone()))
            }
            Body::Generic(_) => return BTreeMap::new(),
        };
        PRIMARY_RAW
            .iter()
            .chain(ALIASES)
            .filter(|(k, _, _)| *k == kind)
            .map(|(_, _, key)| (*key, view.values(key)))
            .filter(|(_, values)| !values.is_empty())
            .collect()
    }

    /// Removes every value at a raw key. Returns whether there was one.
    fn remove_raw(&mut self, kind: TagKind, key: &str) -> Result<bool> {
        Ok(match self {
            Body::Vorbis(comments) => comments.remove(key).count() > 0,
            Body::Ape(tag) => {
                let present = tag.get(key).is_some();
                tag.remove(key);
                present
            }
            Body::Generic(tag) => match kind {
                TagKind::Id3v2 => {
                    let description = txxx(key)?;
                    with_id3v2(tag, |id3| {
                        let mut removed = false;
                        while id3.remove_user_text(description).is_some() {
                            removed = true;
                        }
                        removed
                    })
                }
                TagKind::Mp4Ilst => {
                    let ident = freeform(key).with_context(|| format!("bad MP4 key {key}"))?;
                    with_ilst(tag, |ilst| ilst.remove(&ident).count() > 0)
                }
                _ => bail!("{kind:?} tags have no raw keys"),
            },
        })
    }

    /// Adds a value at a raw key, after any values already there.
    fn push_raw(&mut self, kind: TagKind, key: &str, value: &str) -> Result<()> {
        match self {
            Body::Vorbis(comments) => comments.push(key.to_owned(), value.to_owned()),
            Body::Ape(tag) => tag.push(ApeItem::new(
                key.to_owned(),
                ItemValue::Text(value.to_owned()),
            )?),
            Body::Generic(tag) => match kind {
                TagKind::Id3v2 => {
                    let description = txxx(key)?;
                    with_id3v2(tag, |id3| {
                        let content = match id3.get_user_text(description) {
                            Some(existing) => format!("{existing}\0{value}"),
                            None => value.to_owned(),
                        };
                        id3.insert_user_text(description.to_owned(), content);
                    });
                }
                TagKind::Mp4Ilst => {
                    let ident = freeform(key).with_context(|| format!("bad MP4 key {key}"))?;
                    with_ilst(tag, |ilst| {
                        ilst.insert(Atom::new(ident, AtomData::UTF8(value.to_owned())));
                    });
                }
                _ => bail!("{kind:?} tags have no raw keys"),
            },
        }
        Ok(())
    }
}

/// A tag in a form that shows its raw keys.
enum RawView<'a> {
    Vorbis(&'a VorbisComments),
    Ape(&'a ApeTag),
    Id3v2(Id3v2Tag),
    Ilst(Ilst),
}

impl RawView<'_> {
    fn values(&self, key: &str) -> Vec<String> {
        match self {
            RawView::Vorbis(comments) => comments.get_all(key).map(str::to_owned).collect(),
            RawView::Ape(tag) => tag
                .get(key)
                .and_then(|item| item.value().text())
                .map(|text| text.split('\0').map(str::to_owned).collect())
                .unwrap_or_default(),
            RawView::Id3v2(id3) => {
                let Some(description) = key.strip_prefix("TXXX:") else {
                    return Vec::new();
                };
                id3.into_iter()
                    .filter_map(|frame| match frame {
                        Frame::UserText(text) if text.description == description => {
                            Some(text.content.split('\0').map(str::to_owned))
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect()
            }
            RawView::Ilst(ilst) => {
                let Some(ident) = freeform(key) else {
                    return Vec::new();
                };
                ilst.get(&ident)
                    .into_iter()
                    .flat_map(Atom::data)
                    .filter_map(|data| match data {
                        AtomData::UTF8(text) | AtomData::UTF16(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            }
        }
    }
}

fn txxx(key: &str) -> Result<&str> {
    key.strip_prefix("TXXX:")
        .with_context(|| format!("bad ID3v2 key {key}"))
}

/// `----:<mean>:<name>` as an MP4 freeform identifier.
fn freeform(key: &str) -> Option<AtomIdent<'static>> {
    let (mean, name) = key.strip_prefix("----:")?.split_once(':')?;
    Some(AtomIdent::Freeform {
        mean: mean.to_owned().into(),
        name: name.to_owned().into(),
    })
}

/// Edits a generic ID3v2 tag's frames, unmapped ones included.
fn with_id3v2<R>(tag: &mut Tag, edit: impl FnOnce(&mut Id3v2Tag) -> R) -> R {
    let mut id3 = Id3v2Tag::from(std::mem::replace(tag, Tag::new(TagType::Id3v2)));
    let result = edit(&mut id3);
    *tag = Tag::from(id3);
    result
}

/// Edits a generic MP4 tag's atoms, unmapped ones included.
fn with_ilst<R>(tag: &mut Tag, edit: impl FnOnce(&mut Ilst) -> R) -> R {
    let mut ilst = Ilst::from(std::mem::replace(tag, Tag::new(TagType::Mp4Ilst)));
    let result = edit(&mut ilst);
    *tag = Tag::from(ilst);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use lofty::id3::v1::Id3v1Tag;
    use lofty::id3::v2::UniqueFileIdentifierFrame;
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

    #[test]
    fn id3_recording_id_is_written_as_txxx_and_ufid_is_removed() {
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
        let musicbrainz_ufids = |id3: &Id3v2Tag, owner: &str| {
            id3.into_iter()
                .filter(|frame| {
                    matches!(frame, Frame::UniqueFileIdentifier(ufid) if ufid.owner == owner)
                })
                .count()
        };
        let before = read_mpeg(&path);
        assert_eq!(
            musicbrainz_ufids(before.id3v2().unwrap(), "http://musicbrainz.org"),
            1
        );
        assert!(read_tags(&path).unwrap().copies.contains(&TagCopy {
            tag: TagKind::Id3v2,
            field: TagField::MusicbrainzRecordingId,
            key: CopyKey::Field,
            value: "old-recording".to_owned(),
            read_only: false,
        }));

        let new = &values()[&TagField::MusicbrainzRecordingId];
        edit_tags(&path, &setting(&[(TagField::MusicbrainzRecordingId, new)])).unwrap();
        let mpeg = read_mpeg(&path);
        let id3 = mpeg.id3v2().unwrap();
        assert_eq!(
            id3.get_user_text("MusicBrainz Track Id"),
            Some(new.as_str())
        );
        assert_eq!(musicbrainz_ufids(id3, "http://musicbrainz.org"), 0);
        assert_eq!(musicbrainz_ufids(id3, "http://example.com"), 1);
        assert_eq!(
            probe(&path).musicbrainz_recording_id.as_deref(),
            Some(new.as_str())
        );
        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.primary[&TagField::MusicbrainzRecordingId], *new);
        assert!(tags.copies.is_empty(), "{:?}", tags.copies);
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
        id3.dump_to(&mut bytes, WriteOptions::default()).unwrap();
        bytes.extend(fs::read(&path).unwrap());
        fs::write(&path, &bytes).unwrap();

        let tags = read_tags(&path).unwrap();
        assert_eq!(tags.format, WritableFormat::Flac);
        assert!(tags.copies.contains(&TagCopy {
            tag: TagKind::Id3v2,
            field: TagField::Title,
            key: CopyKey::Field,
            value: "Old Title".to_owned(),
            read_only: true,
        }));
        assert!(edit_tags(&path, &setting(&[(TagField::Title, "New Title")])).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
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
}
