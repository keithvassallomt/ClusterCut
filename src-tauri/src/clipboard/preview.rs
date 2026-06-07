//! Light-weight previews sent to the frontend in place of the full
//! `ClipboardPayload`. The frontend renders a 3-line text clamp or a small
//! image thumbnail and re-calls full content by id — so it never needs the
//! whole payload across the IPC bridge.

use crate::clipboard::history_store::StoredContent;
use crate::protocol::ClipboardPayload;
use base64::Engine as _;
use serde::Serialize;
use std::io::Cursor;

/// Max bytes of text shipped for the preview. Enough to fill the 3-line clamp
/// with headroom; truncated on a UTF-8 char boundary.
pub const TEXT_PREVIEW_BYTES: usize = 4096;

/// Max edge (px) of the generated image thumbnail.
pub const THUMB_MAX_EDGE: u32 = 256;

#[derive(Serialize, Clone, Debug)]
pub struct BlobPreview {
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: u64,
    /// base64 PNG thumbnail, or None for a not-yet-fetched descriptor.
    pub thumbnail: Option<String>,
    /// True when bytes aren't available yet (receiver descriptor pre-fetch).
    pub descriptor: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct FormatPreview {
    pub mime_type: String,
    pub binary: bool,
    /// Wire-encoded byte length (base64 for binary formats), matching
    /// `ClipboardFormat.data.len()` accounting — not the raw decoded size.
    pub size: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct ClipboardPreview {
    pub id: String,
    pub sender: String,
    pub sender_id: String,
    pub timestamp: u64,
    /// Truncated text (text/rich items). None for image-only items.
    pub text_preview: Option<String>,
    /// True byte length of the full text (for "31.0 MB" display).
    pub text_len: u64,
    pub blob: Option<BlobPreview>,
    pub formats: Option<Vec<FormatPreview>>,
    pub files: Option<Vec<crate::protocol::FileMetadata>>,
    /// Whether re-call is currently possible (content still in the store).
    pub has_backing: bool,
}

/// UTF-8-safe truncation to TEXT_PREVIEW_BYTES.
pub fn text_preview_str(s: &str) -> String {
    if s.len() <= TEXT_PREVIEW_BYTES {
        return s.to_string();
    }
    let mut end = TEXT_PREVIEW_BYTES;
    // Walk back to the nearest char boundary. `end` reaching 0 (empty result)
    // is only possible for a degenerate string with no char boundary within the
    // first TEXT_PREVIEW_BYTES bytes — impossible for valid UTF-8 — and is
    // handled gracefully here rather than panicking.
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Decode an image and emit a small base64 PNG thumbnail. Returns None if the
/// bytes don't decode as an image.
pub fn make_thumbnail(bytes: &[u8]) -> Option<String> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(THUMB_MAX_EDGE, THUMB_MAX_EDGE);
    let mut out = Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(out.get_ref()))
}

/// (text_preview, text_len, blob_preview) for a backed StoredContent.
/// `thumbnail` is computed by the caller (it has the bytes / file).
pub fn preview_parts(
    content: &StoredContent,
    thumbnail: Option<String>,
) -> (Option<String>, u64, Option<BlobPreview>) {
    match content {
        StoredContent::Text(s) => (Some(text_preview_str(s)), s.len() as u64, None),
        StoredContent::Rich { text, .. } => {
            (Some(text_preview_str(text)), text.len() as u64, None)
        }
        StoredContent::Image {
            mime,
            bytes,
            width,
            height,
        } => (
            None,
            0,
            Some(BlobPreview {
                mime_type: mime.clone(),
                width: *width,
                height: *height,
                size: bytes.len() as u64,
                thumbnail,
                descriptor: false,
            }),
        ),
        StoredContent::Disk {
            mime,
            width,
            height,
            size,
            ..
        } => {
            if mime.starts_with("text/") {
                // Large text: text_preview is returned as None here because no
                // text bytes are available in this match. CALLER PRECONDITION:
                // for a Disk text entry the caller MUST fill text_preview from
                // the staged-file prefix before emitting. text_len is the true
                // byte size.
                (None, *size, None)
            } else {
                (
                    None,
                    0,
                    Some(BlobPreview {
                        mime_type: mime.clone(),
                        width: *width,
                        height: *height,
                        size: *size,
                        thumbnail,
                        descriptor: true,
                    }),
                )
            }
        }
    }
}

/// Build the descriptor preview for a payload with NO backing yet (receiver
/// pending / fetching). Large-text descriptors render as text (size only);
/// image descriptors render as a thumbnail-less blob.
pub fn descriptor_preview(payload: &ClipboardPayload) -> (Option<String>, u64, Option<BlobPreview>) {
    if let Some(b) = payload.blob.as_ref() {
        if b.mime_type.starts_with("text/") {
            return (None, b.total_size.unwrap_or(0), None);
        }
        return (
            None,
            0,
            Some(BlobPreview {
                mime_type: b.mime_type.clone(),
                width: b.width,
                height: b.height,
                size: b.total_size.unwrap_or(0),
                thumbnail: None,
                descriptor: true,
            }),
        );
    }
    // No blob: inline text/rich with no backing only happens for empty content.
    (
        if payload.text.is_empty() {
            None
        } else {
            Some(text_preview_str(&payload.text))
        },
        payload.text.len() as u64,
        None,
    )
}

/// Build the light format summary (no bytes).
pub fn formats_preview(payload: &ClipboardPayload) -> Option<Vec<FormatPreview>> {
    payload.formats.as_ref().filter(|f| !f.is_empty()).map(|fs| {
        fs.iter()
            .map(|f| FormatPreview {
                mime_type: f.mime_type.clone(),
                binary: f.binary,
                size: f.data.len() as u64,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_preview_passes_short_text() {
        assert_eq!(text_preview_str("hello"), "hello");
    }

    #[test]
    fn text_preview_truncates_long_text_on_char_boundary() {
        let s = "é".repeat(5000); // 2 bytes each = 10000 bytes
        let p = text_preview_str(&s);
        assert!(p.len() <= TEXT_PREVIEW_BYTES);
        // Did not split a multi-byte char:
        assert!(p.chars().all(|c| c == 'é'));
    }

    #[test]
    fn thumbnail_of_small_png_is_bounded() {
        // 1x1 transparent PNG.
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let t = make_thumbnail(png);
        assert!(t.is_some());
        assert!(!t.unwrap().is_empty());
    }

    #[test]
    fn thumbnail_of_garbage_is_none() {
        assert!(make_thumbnail(b"not an image").is_none());
    }

    #[test]
    fn thumbnail_caps_dimensions() {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(512, 384, Rgba([10, 20, 30, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let b64 = make_thumbnail(png.get_ref()).expect("thumbnail");
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert!(decoded.width() <= 256 && decoded.height() <= 256);
    }
}
