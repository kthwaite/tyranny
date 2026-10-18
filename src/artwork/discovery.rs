use super::{Images, provider::MAX_SOURCE_BYTES};
use crate::protocol::{Artwork, ArtworkSource, ArtworkStatus};
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ffmpeg_next as ffmpeg;
use image::{ImageReader, Limits, codecs::jpeg::JpegEncoder};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Cursor, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

const MAX_PIXELS: u64 = 16_000_000;
const MAX_DIMENSION: u32 = 8192;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const FOLDER_NAMES: [&str; 6] = [
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
];

pub(super) fn normalize(bytes: &[u8], source: ArtworkSource) -> Result<Images> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_SOURCE_BYTES,
        "image source exceeds byte limit or is empty"
    );
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (width, height) = reader.into_dimensions()?;
    ensure!(
        width > 0 && height > 0 && width <= MAX_DIMENSION && height <= MAX_DIMENSION,
        "image dimensions exceed limit"
    );
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "image pixel count exceeds limit"
    );
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode()?;
    let display = if width > 512 || height > 512 {
        decoded.resize(512, 512, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    if display.width() <= 128 && display.height() <= 128 {
        let display = encode(display, source)?;
        return Ok(Images {
            thumbnail: display.clone(),
            display,
        });
    }
    let thumbnail = display.resize(128, 128, image::imageops::FilterType::Triangle);
    Ok(Images {
        thumbnail: encode(thumbnail, source)?,
        display: encode(display, source)?,
    })
}
fn encode(image: image::DynamicImage, source: ArtworkSource) -> Result<Artwork> {
    let width = image.width();
    let height = image.height();
    let rgb = image.into_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 85).encode(
        rgb.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )?;
    ensure!(
        bytes.len() <= MAX_RESPONSE_BYTES,
        "normalized image exceeds response limit"
    );
    Ok(Artwork {
        status: ArtworkStatus::Ready,
        source: Some(source),
        cache_key: Some(format!("{:x}", Sha256::digest(&bytes))),
        width,
        height,
        mime_type: Some("image/jpeg".into()),
        data: Some(STANDARD.encode(bytes)),
        message: None,
    })
}
fn check(stop: &AtomicBool, deadline: Instant) -> Result<()> {
    ensure!(!stop.load(Ordering::Acquire), "artwork discovery cancelled");
    ensure!(
        Instant::now() < deadline,
        "artwork discovery deadline exceeded"
    );
    Ok(())
}
pub(super) fn local(
    paths: &[PathBuf],
    stop: &Arc<AtomicBool>,
    deadline: Instant,
) -> Result<(Option<Images>, Option<String>)> {
    let mut failure = None;
    // Finish the embedded tier for the whole album before considering any folder.
    for path in paths {
        check(stop, deadline)?;
        match embedded(path, stop, deadline) {
            Ok(Some(images)) => return Ok((Some(images), None)),
            Ok(None) => {}
            Err(error) => failure = Some(format!("{error:#}")),
        }
    }
    let directories: BTreeSet<_> = paths.iter().filter_map(|path| path.parent()).collect();
    for directory in directories {
        check(stop, deadline)?;
        match folder_candidates(directory, stop, deadline) {
            Ok(candidates) => {
                for path in candidates {
                    check(stop, deadline)?;
                    match read_image(&path)
                        .and_then(|bytes| normalize(&bytes, ArtworkSource::Folder))
                    {
                        Ok(images) => return Ok((Some(images), None)),
                        Err(error) => failure = Some(format!("{error:#}")),
                    }
                }
            }
            Err(error) => failure = Some(format!("{error:#}")),
        }
    }
    check(stop, deadline)?;
    Ok((None, failure))
}
fn read_image(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "folder artwork must be a regular file");
    ensure!(
        metadata.len() <= MAX_SOURCE_BYTES as u64,
        "folder artwork exceeds source byte limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_SOURCE_BYTES,
        "folder artwork grew beyond source byte limit"
    );
    Ok(bytes)
}
fn folder_candidates(
    directory: &Path,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        check(stop, deadline)?;
        ensure!(index < 100_000, "artwork directory exceeds entry limit");
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(rank) = FOLDER_NAMES
            .iter()
            .position(|candidate| name.eq_ignore_ascii_case(candidate))
        {
            ensure!(
                candidates.len() < 128,
                "artwork directory has too many cover candidates"
            );
            // Reject links, directories and special files before opening (in
            // particular a named pipe must never pin one of the image workers).
            if entry.file_type()?.is_file() {
                candidates.push((rank, name.to_owned(), entry.path()));
            }
        }
    }
    candidates.sort_unstable_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    Ok(candidates.into_iter().map(|(_, _, path)| path).collect())
}
fn embedded(path: &Path, stop: &Arc<AtomicBool>, deadline: Instant) -> Result<Option<Images>> {
    check(stop, deadline)?;
    ensure!(path.is_absolute(), "artwork source must be absolute");
    ensure!(
        fs::metadata(path)?.is_file(),
        "artwork source must be a regular file"
    );
    crate::decoder::initialize_ffmpeg()?;
    let interrupt = Arc::clone(stop);
    let mut options = ffmpeg::Dictionary::new();
    options.set("probesize", "2097152");
    options.set("analyzeduration", "2000000");
    options.set("max_probe_packets", "256");
    options.set("max_streams", "64");
    options.set("skip_estimate_duration_from_pts", "1");
    options.set("protocol_whitelist", "file");
    let input = ffmpeg::format::input_with_interrupt_and_dictionary(
        path,
        move || interrupt.load(Ordering::Acquire) || Instant::now() >= deadline,
        options,
    )?;
    check(stop, deadline)?;
    let mut invalid = None;
    for stream in input.streams() {
        // AVStream owns attached_pic for the lifetime of input. Borrow its
        // packet only after checking its signed length, never clone a packet or
        // allocate an unbounded attachment buffer.
        let stream = unsafe { &*stream.as_ptr() };
        let packet = &stream.attached_pic;
        if packet.size <= 0 || packet.data.is_null() {
            continue;
        }
        if packet.size as usize > MAX_SOURCE_BYTES {
            invalid = Some(anyhow::anyhow!(
                "embedded artwork exceeds source byte limit"
            ));
            continue;
        }
        let bytes = unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) };
        match normalize(bytes, ArtworkSource::Embedded) {
            Ok(images) => return Ok(Some(images)),
            Err(error) => invalid = Some(error.context("invalid embedded artwork")),
        }
    }
    match invalid {
        Some(error) => Err(error),
        None => Ok(None),
    }
}
