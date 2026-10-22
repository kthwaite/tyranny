//! Writes tags through a clone, then renames the clone over the original.
//!
//! The original is never opened for writing. A playing decoder keeps the old
//! inode open, so it keeps decoding the old bytes.
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::tags::{TagCopy, TagEdit, edit_tags_file, read_tags_file};

/// Size and modification time of a file, taken when it was looked up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileStamp {
    pub size: u64,
    pub modified_ns: i64,
}

#[derive(Debug)]
pub(crate) enum WriteError {
    /// The file differs from the stamp, or is gone.
    Changed,
    Failed(String),
}

impl From<anyhow::Error> for WriteError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(format!("{error:#}"))
    }
}

/// The stamp of a regular file. A symlink is not followed.
pub(crate) fn stamp(path: &Path) -> std::io::Result<FileStamp> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let modified_ns = match metadata.modified()?.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_nanos()).unwrap_or(i64::MAX),
    };
    Ok(FileStamp {
        size: metadata.len(),
        modified_ns,
    })
}

/// `<dir>/.<file name>.tyranny-likes-smoke-tmp`
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tyranny-likes-smoke-tmp");
    path.with_file_name(name)
}

/// Applies `edit` to a clone of `path` and renames the clone over it.
/// Returns the copies the edit removed.
pub(crate) fn write_safely(
    path: &Path,
    expected: FileStamp,
    edit: &TagEdit,
) -> Result<Vec<TagCopy>, WriteError> {
    write_with(path, expected, edit, |_| Ok(()))
}

/// `Ok(false)` when the file is gone or differs from the stamp. Other I/O errors are errors.
fn unchanged(path: &Path, expected: FileStamp) -> Result<bool> {
    match stamp(path) {
        Ok(current) => Ok(current == expected),
        // Gone, or replaced by a symlink or directory (`stamp` says `InvalidInput`).
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput) => {
            Ok(false)
        }
        Err(error) => Err(anyhow!("check file: {error}")),
    }
}

/// `before_commit` runs on the edited clone, before it is flushed.
fn write_with(
    path: &Path,
    expected: FileStamp,
    edit: &TagEdit,
    before_commit: impl FnOnce(&Path) -> Result<()>,
) -> Result<Vec<TagCopy>, WriteError> {
    write_with_hooks(path, expected, edit, |_| Ok(()), before_commit)
}

/// `after_create` runs once the empty clone exists, before anything is copied into it.
fn write_with_hooks(
    path: &Path,
    expected: FileStamp,
    edit: &TagEdit,
    after_create: impl FnOnce(&Path) -> Result<()>,
    before_commit: impl FnOnce(&Path) -> Result<()>,
) -> Result<Vec<TagCopy>, WriteError> {
    if !unchanged(path, expected)? {
        return Err(WriteError::Changed);
    }
    let temp = temp_path(path);
    let mut created = false;
    let result = clone_edit_and_commit(
        path,
        expected,
        edit,
        &temp,
        &mut created,
        after_create,
        before_commit,
    );
    // Only remove a file this call created. It is a regular file, never a followed link.
    if result.is_err() && created {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Creates the clone exclusively (never through a symlink), private to the owner.
fn create_clone(temp: &Path) -> Result<File> {
    // A stale temporary file, or a link planted there, is removed itself, never followed.
    match fs::remove_file(temp) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(anyhow!("remove stale temporary file: {error}")),
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(temp)
        .map_err(|error| anyhow!("create temporary file: {error}"))
}

/// Data, permissions and extended attributes.
#[cfg(target_os = "macos")]
fn copy_into(path: &Path, clone: &mut File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let source = File::open(path)?;
    let flags =
        libc::COPYFILE_ACL | libc::COPYFILE_STAT | libc::COPYFILE_XATTR | libc::COPYFILE_DATA;
    // SAFETY: both descriptors are open for the call; a null state is allowed.
    let status = unsafe {
        libc::fcopyfile(
            source.as_raw_fd(),
            clone.as_raw_fd(),
            std::ptr::null_mut(),
            flags,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn copy_into(path: &Path, clone: &mut File) -> std::io::Result<()> {
    let mut source = File::open(path)?;
    std::io::copy(&mut source, clone)?;
    clone.set_permissions(source.metadata()?.permissions())
}

fn clone_edit_and_commit(
    path: &Path,
    expected: FileStamp,
    edit: &TagEdit,
    temp: &Path,
    created: &mut bool,
    after_create: impl FnOnce(&Path) -> Result<()>,
    before_commit: impl FnOnce(&Path) -> Result<()>,
) -> Result<Vec<TagCopy>, WriteError> {
    let mut clone = create_clone(temp)?;
    *created = true;
    let identity = clone.metadata().map_err(|error| anyhow!("{error}"))?;
    // From here on, only this handle is used. The temp is never reopened by name.
    after_create(temp)?;
    copy_into(path, &mut clone).map_err(|error| anyhow!("clone file: {error}"))?;
    let removed = edit_tags_file(&mut clone, edit)?;
    before_commit(temp)?;
    // The edited clone must still parse, so a bad write never replaces a good file.
    read_tags_file(&mut clone).map_err(|error| anyhow!("edited file does not parse: {error:#}"))?;
    clone
        .sync_all()
        .map_err(|error| anyhow!("flush temporary file: {error}"))?;
    let current = fs::symlink_metadata(temp).map_err(|error| anyhow!("check clone: {error}"))?;
    if !current.is_file() || current.dev() != identity.dev() || current.ino() != identity.ino() {
        // Not ours any more: remove nothing.
        *created = false;
        return Err(anyhow!("temporary file was replaced").into());
    }
    if !unchanged(path, expected)? {
        return Err(WriteError::Changed);
    }
    fs::rename(temp, path).map_err(|error| anyhow!("replace file: {error}"))?;
    // The rename is done. A failed directory sync cannot undo it, so it is ignored.
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::{Decoder, GainMode};
    use crate::protocol::TagField;
    use crate::tagging::tags::read_tags;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("tyranny-likes-smoke-safe-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn copy_as(&self, fixture: &str, name: &str) -> PathBuf {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/formats")
                .join(fixture);
            let target = self.0.join(name);
            fs::copy(source, &target).unwrap();
            target
        }

        fn copy(&self, fixture: &str) -> PathBuf {
            self.copy_as(fixture, fixture)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn title(value: &str) -> TagEdit {
        TagEdit {
            set: [(TagField::Title, Some(value.to_owned()))].into(),
            restore: Vec::new(),
        }
    }

    #[test]
    fn failed_write_after_clone_leaves_original_identical_and_no_temp() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let before = fs::read(&path).unwrap();
        let expected = stamp(&path).unwrap();
        let result = write_with(&path, expected, &title("New"), |_| Err(anyhow!("boom")));
        assert!(matches!(result, Err(WriteError::Failed(ref m)) if m.contains("boom")));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn original_changed_before_rename_is_reported_changed() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let mut expected_bytes = fs::read(&path).unwrap();
        expected_bytes.push(0);
        let expected = stamp(&path).unwrap();
        let result = write_with(&path, expected, &title("New"), |_| {
            let mut file = fs::OpenOptions::new().append(true).open(&path)?;
            file.write_all(&[0])?;
            Ok(())
        });
        assert!(matches!(result, Err(WriteError::Changed)));
        assert_eq!(fs::read(&path).unwrap(), expected_bytes);
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn stale_stamp_is_changed_without_touching_file() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let before = fs::read(&path).unwrap();
        let mut expected = stamp(&path).unwrap();
        expected.size += 1;
        let result = write_safely(&path, expected, &title("New"));
        assert!(matches!(result, Err(WriteError::Changed)));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn stale_temp_file_from_a_crash_is_replaced() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        fs::write(temp_path(&path), b"junk from a crash").unwrap();
        let expected = stamp(&path).unwrap();
        write_safely(&path, expected, &title("Fresh")).unwrap();
        assert!(!temp_path(&path).exists());
        assert_eq!(
            read_tags(&path).unwrap().current(TagField::Title),
            Some("Fresh")
        );
    }

    #[test]
    fn unicode_file_names_write() {
        let scratch = Scratch::new();
        let path = scratch.copy_as("tone.flac", "Ünïcødé – 日本 song.flac");
        let expected = stamp(&path).unwrap();
        write_safely(&path, expected, &title("Ünï")).unwrap();
        assert!(!temp_path(&path).exists());
        assert_eq!(
            read_tags(&path).unwrap().current(TagField::Title),
            Some("Ünï")
        );
    }

    #[test]
    fn permissions_survive_and_unwritable_directory_fails_cleanly() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let expected = stamp(&path).unwrap();
        write_safely(&path, expected, &title("One")).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);

        let before = fs::read(&path).unwrap();
        let expected = stamp(&path).unwrap();
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o555)).unwrap();
        let result = write_safely(&path, expected, &title("Two"));
        let temp_exists = temp_path(&path).exists();
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(WriteError::Failed(_))), "{result:?}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!temp_exists);
    }

    #[test]
    fn deleted_file_is_changed() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let expected = stamp(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            write_safely(&path, expected, &title("New")),
            Err(WriteError::Changed)
        ));
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn decoder_open_during_write_keeps_decoding() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let mut decoder = Decoder::open(&path, GainMode::Off).unwrap();
        let mut buffer = vec![[0.0f32; 2]; 4096];
        let mut total = decoder.read(&mut buffer).unwrap();
        assert_eq!(total, 4096);
        let expected = stamp(&path).unwrap();
        write_safely(&path, expected, &title("While Playing")).unwrap();
        loop {
            let read = decoder.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            total += read;
        }
        assert_eq!(total, 12000);
    }

    #[test]
    fn symlink_planted_at_temp_path_never_touches_the_victim() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let victim = scratch.0.join("victim.txt");
        fs::write(&victim, b"precious").unwrap();
        std::os::unix::fs::symlink(&victim, temp_path(&path)).unwrap();
        let expected = stamp(&path).unwrap();
        let result = write_safely(&path, expected, &title("Linked"));
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn replaced_temp_fails_cleanly_and_removes_nothing_foreign() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let before = fs::read(&path).unwrap();
        let expected = stamp(&path).unwrap();
        let result = write_with(&path, expected, &title("New"), |temp| {
            fs::remove_file(temp)?;
            fs::copy(&path, temp)?;
            Ok(())
        });
        assert!(matches!(result, Err(WriteError::Failed(_))), "{result:?}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(temp_path(&path)).unwrap(), before);
    }

    #[test]
    fn unparseable_edited_clone_is_not_committed() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let before = fs::read(&path).unwrap();
        let expected = stamp(&path).unwrap();
        let result = write_with(&path, expected, &title("New"), |temp| {
            fs::write(temp, b"not audio")?;
            Ok(())
        });
        assert!(matches!(result, Err(WriteError::Failed(_))), "{result:?}");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn temp_swapped_for_a_symlink_after_creation_fails_and_spares_the_victim() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let victim = scratch.copy_as("tone.flac", "victim.flac");
        let original = fs::read(&path).unwrap();
        let victim_bytes = fs::read(&victim).unwrap();
        let expected = stamp(&path).unwrap();
        let result = write_with_hooks(
            &path,
            expected,
            &title("Raced"),
            |temp| {
                fs::remove_file(temp)?;
                std::os::unix::fs::symlink(&victim, temp)?;
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(matches!(result, Err(WriteError::Failed(_))), "{result:?}");
        assert_eq!(fs::read(&victim).unwrap(), victim_bytes);
        assert_eq!(fs::read(&path).unwrap(), original);
        // The planted link is not ours, so it stays.
        assert!(
            fs::symlink_metadata(temp_path(&path))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn clone_is_private_when_created() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let expected = stamp(&path).unwrap();
        let mut seen = None;
        write_with_hooks(
            &path,
            expected,
            &title("Private"),
            |temp| {
                seen = Some(fs::metadata(temp)?.permissions().mode() & 0o777);
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(seen, Some(0o600));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn original_replaced_by_a_symlink_is_changed() {
        let scratch = Scratch::new();
        let path = scratch.copy("tone.flac");
        let other = scratch.copy_as("tone.flac", "other.flac");
        let expected = stamp(&path).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(matches!(
            write_safely(&path, expected, &title("New")),
            Err(WriteError::Changed)
        ));
    }
}
