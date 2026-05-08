// Deterministic rules for whether a file is worth compressing on the wire.
//
// Compression is applied only when:
//   1. The user has enabled `compress_file_transfers` in settings (checked at the call site).
//   2. The file is large enough that compression overhead is amortised (>= 64 KB).
//   3. The file's extension isn't in the deny-list of formats that are already
//      compressed (recompressing them wastes CPU for ~0% gain).

pub const MIN_COMPRESSIBLE_BYTES: u64 = 64 * 1024;

const ALREADY_COMPRESSED_EXTENSIONS: &[&str] = &[
    // Images
    "jpg", "jpeg", "png", "webp", "heic", "heif", "gif", "avif",
    // Video
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "wmv",
    // Audio
    "mp3", "m4a", "aac", "ogg", "flac", "opus", "wma",
    // Archives & compressed formats
    "zip", "7z", "gz", "tgz", "bz2", "tbz2", "xz", "zst", "br", "lz4", "rar", "lz", "lzma",
    // Office (zip-based)
    "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub",
    // Packages / disk images
    "apk", "jar", "war", "deb", "rpm", "appimage", "dmg", "iso", "msi",
];

pub fn should_compress(file_name: &str, file_size: u64) -> bool {
    if file_size < MIN_COMPRESSIBLE_BYTES {
        return false;
    }
    if let Some(ext) = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
    {
        let lower = ext.to_ascii_lowercase();
        if ALREADY_COMPRESSED_EXTENSIONS.contains(&lower.as_str()) {
            return false;
        }
    }
    true
}

pub const ZSTD_LEVEL: i32 = 3;
