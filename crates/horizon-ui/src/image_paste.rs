//! Clipboard image paste support.
//!
//! When the user pastes (Ctrl+V / platform paste shortcut) while the clipboard
//! holds an IMAGE rather than text, Horizon saves the image as a PNG under
//! [`HorizonHome::pasted_dir`](horizon_core::HorizonHome::pasted_dir) and feeds
//! the resulting file PATH (as text) into the focused terminal. This lets CLIs
//! that accept image paths (for example Claude Code) receive the image.
//!
//! egui-winit only emits [`egui::Event::Paste`] when the clipboard holds
//! non-empty TEXT, so an image-only clipboard produces no egui paste event at
//! all. We therefore detect the paste keystroke from the raw observed keyboard
//! queue inside the app's `raw_input_hook`, consult the clipboard there, and —
//! when an image is present — rewrite the frame's input so a single
//! [`egui::Event::Paste`] carrying the saved PNG file PATH flows through the
//! ordinary downstream paste handler. Rewriting the event (rather than adding a
//! parallel write path) is what guarantees there is never a double paste: the
//! text paste, if any, is replaced by the image-path paste.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Error encoding raw RGBA8 pixels into a PNG byte buffer.
#[derive(Debug)]
pub(crate) enum PngEncodeError {
    /// The pixel buffer length did not match `width * height * 4`.
    PixelBufferSize { expected: usize, actual: usize },
    /// The image had zero width or height.
    EmptyDimensions,
    /// The `png` encoder reported a failure.
    Encode(String),
}

impl std::fmt::Display for PngEncodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PixelBufferSize { expected, actual } => {
                write!(
                    formatter,
                    "pixel buffer size mismatch: expected {expected}, got {actual}"
                )
            }
            Self::EmptyDimensions => write!(formatter, "image has zero width or height"),
            Self::Encode(message) => write!(formatter, "png encode failed: {message}"),
        }
    }
}

impl std::error::Error for PngEncodeError {}

/// Encode raw RGBA8 pixels (row-major, 4 bytes per pixel) into an in-memory PNG.
///
/// This is the pure, testable core of the feature: it has no clipboard or
/// filesystem dependency.
pub(crate) fn encode_rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, PngEncodeError> {
    if width == 0 || height == 0 {
        return Err(PngEncodeError::EmptyDimensions);
    }

    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(PngEncodeError::EmptyDimensions)?;
    if rgba.len() != expected {
        return Err(PngEncodeError::PixelBufferSize {
            expected,
            actual: rgba.len(),
        });
    }

    let width_u32 = u32::try_from(width).map_err(|error| PngEncodeError::Encode(error.to_string()))?;
    let height_u32 = u32::try_from(height).map_err(|error| PngEncodeError::Encode(error.to_string()))?;

    let mut buffer = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buffer, width_u32, height_u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| PngEncodeError::Encode(error.to_string()))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| PngEncodeError::Encode(error.to_string()))?;
    }

    Ok(buffer)
}

/// Build the file name for a pasted clipboard image, derived from `now`.
///
/// The name is millisecond-resolution to keep rapid successive pastes from
/// colliding, and falls back to `0` for clocks before the Unix epoch.
pub(crate) fn pasted_png_filename(now: SystemTime) -> String {
    let millis = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("clipboard-{millis}.png")
}

/// Encode `rgba` to PNG and write it under `pasted_dir`, returning the path.
pub(crate) fn write_pasted_png(
    pasted_dir: &Path,
    now: SystemTime,
    width: usize,
    height: usize,
    rgba: &[u8],
) -> Result<PathBuf, ImagePasteError> {
    let png_bytes = encode_rgba_to_png(width, height, rgba)?;
    std::fs::create_dir_all(pasted_dir).map_err(ImagePasteError::Io)?;
    let path = pasted_dir.join(pasted_png_filename(now));
    std::fs::write(&path, png_bytes).map_err(ImagePasteError::Io)?;
    Ok(path)
}

/// Failure while turning a clipboard image into an on-disk PNG path.
#[derive(Debug)]
pub(crate) enum ImagePasteError {
    Encode(PngEncodeError),
    Io(std::io::Error),
}

impl From<PngEncodeError> for ImagePasteError {
    fn from(error: PngEncodeError) -> Self {
        Self::Encode(error)
    }
}

impl std::fmt::Display for ImagePasteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "io error: {error}"),
        }
    }
}

impl std::error::Error for ImagePasteError {}

/// If the system clipboard currently holds an image, save it as a PNG under
/// `pasted_dir` and return the file path. Returns `None` when there is no image
/// (so the normal text paste flow must proceed unchanged) or when any step
/// fails — every failure mode degrades gracefully via tracing.
///
/// The clipboard read is only wired up on Linux today (matching
/// [`crate::primary_selection`]); other platforms always return `None` so the
/// existing text paste behavior is preserved verbatim.
#[cfg(target_os = "linux")]
pub(crate) fn read_clipboard_image_to_png(pasted_dir: &Path) -> Option<PathBuf> {
    use arboard::Clipboard;

    let mut clipboard = match Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            tracing::debug!("clipboard unavailable for image paste: {error}");
            return None;
        }
    };

    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return None,
        Err(error) => {
            tracing::debug!("clipboard image read failed: {error}");
            return None;
        }
    };

    match write_pasted_png(pasted_dir, SystemTime::now(), image.width, image.height, &image.bytes) {
        Ok(path) => Some(path),
        Err(error) => {
            tracing::warn!("failed to save pasted clipboard image: {error}");
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn read_clipboard_image_to_png(pasted_dir: &Path) -> Option<PathBuf> {
    let _ = pasted_dir;
    None
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{ImagePasteError, PngEncodeError, encode_rgba_to_png, pasted_png_filename, write_pasted_png};

    fn solid_rgba(width: usize, height: usize) -> Vec<u8> {
        vec![0x7f; width * height * 4]
    }

    #[test]
    fn encode_produces_valid_png_signature() {
        let png = encode_rgba_to_png(2, 2, &solid_rgba(2, 2)).expect("encode 2x2");

        // PNG magic number / signature.
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // The IHDR chunk type follows the 8-byte length field after the signature.
        assert_eq!(&png[12..16], b"IHDR");
    }

    #[test]
    fn encode_round_trips_through_decoder() {
        let png = encode_rgba_to_png(3, 1, &solid_rgba(3, 1)).expect("encode 3x1");

        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let reader = decoder.read_info().expect("read info");
        let info = reader.info();
        assert_eq!(info.width, 3);
        assert_eq!(info.height, 1);
        assert_eq!(info.color_type, png::ColorType::Rgba);
    }

    #[test]
    fn encode_rejects_pixel_buffer_size_mismatch() {
        let error = encode_rgba_to_png(2, 2, &[0u8; 4]).expect_err("size mismatch");
        match error {
            PngEncodeError::PixelBufferSize { expected, actual } => {
                assert_eq!(expected, 16);
                assert_eq!(actual, 4);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn encode_rejects_empty_dimensions() {
        assert!(matches!(
            encode_rgba_to_png(0, 4, &[]),
            Err(PngEncodeError::EmptyDimensions)
        ));
    }

    #[test]
    fn filename_uses_clipboard_prefix_and_png_extension() {
        let name = pasted_png_filename(SystemTime::UNIX_EPOCH + Duration::from_millis(1_234));
        assert_eq!(name, "clipboard-1234.png");
    }

    #[test]
    fn filename_falls_back_to_zero_before_epoch() {
        let name = pasted_png_filename(SystemTime::UNIX_EPOCH - Duration::from_millis(5));
        assert_eq!(name, "clipboard-0.png");
    }

    #[test]
    fn write_pasted_png_creates_file_under_pasted_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pasted_dir = dir.path().join("pasted");
        let now = SystemTime::UNIX_EPOCH + Duration::from_millis(42);

        let path = write_pasted_png(&pasted_dir, now, 2, 2, &solid_rgba(2, 2)).expect("write png");

        assert_eq!(path, pasted_dir.join("clipboard-42.png"));
        let written = std::fs::read(&path).expect("read back");
        assert_eq!(&written[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    }

    #[test]
    fn write_pasted_png_surfaces_encode_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = write_pasted_png(dir.path(), SystemTime::now(), 0, 0, &[]).expect_err("encode error");
        assert!(matches!(
            error,
            ImagePasteError::Encode(PngEncodeError::EmptyDimensions)
        ));
    }
}
