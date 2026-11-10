//! Bounded JSON framing and filesystem/peer checks shared by local controllers.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, Metadata},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Component, Path, PathBuf},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
};

pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Queue pages can contain 500 paths, so replies have a separate finite ceiling.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Partial bytes live here, not in the read future: cancelling `read` in a select
/// never loses a fragment. A framing error is terminal for the connection.
#[derive(Default)]
pub struct FrameReader {
    header: [u8; 4],
    header_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
}

impl FrameReader {
    pub async fn read<'a>(
        &'a mut self,
        socket: &mut (impl AsyncRead + Unpin),
        maximum: usize,
    ) -> io::Result<Option<&'a [u8]>> {
        while self.header_read < 4 {
            let count = socket.read(&mut self.header[self.header_read..]).await?;
            if count == 0 {
                return if self.header_read == 0 {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
            self.header_read += count;
        }
        let length = u32::from_be_bytes(self.header) as usize;
        if length == 0 || length > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid frame length",
            ));
        }
        self.payload.resize(length, 0);
        while self.payload_read < length {
            let count = socket.read(&mut self.payload[self.payload_read..]).await?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.payload_read += count;
        }
        self.header_read = 0;
        self.payload_read = 0;
        Ok(Some(&self.payload))
    }
}

/// A cancelled or failed write must be followed by disconnect, never replay.
pub async fn write_frame(
    socket: &mut (impl AsyncWrite + Unpin),
    payload: &[u8],
    maximum: usize,
) -> io::Result<()> {
    if payload.is_empty() || payload.len() > maximum || payload.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid frame length",
        ));
    }
    socket
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    socket.write_all(payload).await
}

pub fn validate_socket_path(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().as_bytes();
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    ensure!(
        !bytes.is_empty() && !bytes.contains(&0) && bytes.len() < address.sun_path.len(),
        "Unix socket path is empty, contains NUL, or exceeds the platform byte limit"
    );
    Ok(())
}

pub(crate) fn validate_directory(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "state directory must be a real directory, not a symlink"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "state directory must be owned by the current user"
    );
    ensure!(
        metadata.mode() & 0o7777 == 0o700,
        "state directory permissions must be 0700"
    );
    Ok(())
}

/// A private leaf is not sufficient if another user can rename its ancestors.
/// The caller supplies a canonical directory, preserving root-owned sticky
/// temporary roots while rejecting writable or foreign-owned intermediates.
pub(crate) fn validate_ancestors(directory: &Path) -> Result<()> {
    for ancestor in directory.ancestors().skip(1) {
        validate_trusted(ancestor)?;
    }
    Ok(())
}

fn validate_trusted(directory: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(directory)
        .with_context(|| format!("inspect socket ancestor {}", directory.display()))?;
    let root_sticky = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && (metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() })
            && (metadata.mode() & 0o022 == 0 || root_sticky),
        "socket ancestor must be a trusted directory without unsafe write permissions: {}",
        directory.display()
    );
    Ok(())
}

/// The canonical path is not the only path that matters: a symlink in a
/// directory another user can write can be replaced to send the path given
/// elsewhere. Resolve `path` one name at a time, as the kernel does, and
/// require every directory a name is looked up in to be trusted and every
/// symlink followed to be owned by root or the current user. The last name is
/// not followed; callers check it with `validate_directory`.
pub(crate) fn validate_lookup(path: &Path) -> Result<()> {
    enum Step {
        Root,
        Parent,
        Name(std::ffi::OsString),
    }
    fn push(steps: &mut Vec<Step>, path: &Path) {
        for component in path.components().rev() {
            match component {
                Component::RootDir => steps.push(Step::Root),
                Component::ParentDir => steps.push(Step::Parent),
                Component::Normal(name) => steps.push(Step::Name(name.to_owned())),
                Component::CurDir | Component::Prefix(_) => {}
            }
        }
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory")?
            .join(path)
    };
    let mut steps = Vec::new();
    push(&mut steps, &absolute);
    let mut resolved = PathBuf::from("/");
    let mut links = 0;
    while let Some(step) = steps.pop() {
        let name = match step {
            Step::Root => {
                resolved = PathBuf::from("/");
                continue;
            }
            Step::Parent => {
                resolved.pop();
                continue;
            }
            Step::Name(name) => name,
        };
        validate_trusted(&resolved)?;
        let next = resolved.join(&name);
        if steps.is_empty() {
            break;
        }
        let metadata = fs::symlink_metadata(&next)
            .with_context(|| format!("inspect path component {}", next.display()))?;
        if metadata.file_type().is_symlink() {
            ensure!(
                metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() },
                "symlink on the path must be owned by root or the current user: {}",
                next.display()
            );
            links += 1;
            ensure!(
                links <= 40,
                "too many symlinks on the path: {}",
                path.display()
            );
            let target = fs::read_link(&next)
                .with_context(|| format!("read path symlink {}", next.display()))?;
            push(&mut steps, &target);
        } else {
            resolved = next;
        }
    }
    Ok(())
}

pub(crate) fn socket_metadata(path: &Path) -> Result<Metadata> {
    let metadata = fs::symlink_metadata(path).context("inspect control socket")?;
    ensure!(
        metadata.file_type().is_socket(),
        "control socket must be a socket, not a symlink or regular file"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "control socket must be owned by the current user"
    );
    ensure!(
        metadata.mode() & 0o7777 == 0o600,
        "control socket permissions must be 0600"
    );
    Ok(metadata)
}

pub fn verify_peer(socket: &UnixStream) -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    ensure!(
        socket
            .peer_cred()
            .context("read Unix peer credentials")?
            .uid()
            == unsafe { libc::geteuid() },
        "control socket peer must have the same effective UID"
    );
    Ok(())
}

pub(crate) fn prepare_socket(socket: &UnixStream) -> Result<()> {
    verify_peer(socket)?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // Darwin defaults to a 2048-byte send low-water mark. A peer with a
        // smaller receive buffer can accept bytes yet never signal writable,
        // leaving readiness-driven writers stuck before even the frame header.
        let low_water: libc::c_int = 1;
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDLOWAT,
                (&low_water as *const libc::c_int).cast(),
                std::mem::size_of_val(&low_water) as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error())
                .context("configure Unix socket write readiness");
        }
    }
    Ok(())
}

/// Inspect the private containing directory and socket before connecting, then
/// verify both peer credentials and path identity before sending any command.
pub async fn connect(path: &Path) -> Result<UnixStream> {
    ensure!(path.is_absolute(), "control socket path must be absolute");
    let normalized: PathBuf = path.components().collect();
    let parent = normalized
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    validate_directory(&fs::symlink_metadata(parent).context("inspect socket directory")?)?;
    validate_lookup(parent)?;
    let parent = fs::canonicalize(parent).context("resolve socket directory")?;
    validate_directory(&fs::symlink_metadata(&parent)?)?;
    validate_ancestors(&parent)?;
    let normalized = parent.join(
        normalized
            .file_name()
            .context("socket path needs a filename")?,
    );
    validate_socket_path(&normalized)?;
    let before = socket_metadata(&normalized)?;
    let socket = UnixStream::connect(&normalized)
        .await
        .context("connect local control socket")?;
    prepare_socket(&socket)?;
    let after = socket_metadata(&normalized)?;
    ensure!(
        before.dev() == after.dev() && before.ino() == after.ino(),
        "control socket changed during connection"
    );
    Ok(socket)
}
