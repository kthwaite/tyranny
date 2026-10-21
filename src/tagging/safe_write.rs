//! Writes tags through a clone, then renames the clone over the original.
//!
//! The original is never opened for writing. A playing decoder keeps the old
//! inode open, so it keeps decoding the old bytes.
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::tags::{TagCopy, TagEdit, edit_tags};

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

/// `<dir>/.<file name>.tyranny-tmp`
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tyranny-tmp");
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

fn unchanged(path: &Path, expected: FileStamp) -> bool {
    stamp(path).is_ok_and(|current| current == expected)
}

/// `before_commit` runs on the edited clone, before it is flushed.
fn write_with(
    path: &Path,
    expected: FileStamp,
    edit: &TagEdit,
    before_commit: impl FnOnce(&Path) -> Result<()>,
) -> Result<Vec<TagCopy>, WriteError> {
    if !unchanged(path, expected) {
        return Err(WriteError::Changed);
    }
    let temp = temp_path(path);
    let result = clone_edit_and_commit(path, expected, edit, &temp, before_commit);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn clone_edit_and_commit(
    path: &Path,
    expected: FileStamp,
    edit: &TagEdit,
    temp: &Path,
    before_commit: impl FnOnce(&Path) -> Result<()>,
) -> Result<Vec<TagCopy>, WriteError> {
    match fs::remove_file(temp) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(anyhow!("remove stale temporary file: {error}").into()),
    }
    fs::copy(path, temp).map_err(|error| anyhow!("clone file: {error}"))?;
    let removed = edit_tags(temp, edit)?;
    before_commit(temp)?;
    File::open(temp)
        .and_then(|file| file.sync_all())
        .map_err(|error| anyhow!("flush temporary file: {error}"))?;
    if !unchanged(path, expected) {
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
            let path = std::env::temp_dir().join(format!("tyranny-safe-{}", uuid::Uuid::new_v4()));
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
}
