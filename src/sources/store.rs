//! Direct `/nix/store` path inspection.
//!
//! Exposes ls/read over an explicit absolute store path. Paths are resolved and
//! must stay under `/nix/store/`; binary files are refused with a size-annotated
//! error. No `nix` subprocess is required.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

use crate::config::MAX_FILE_SIZE;
use crate::text::{error, format_size, py_repr};

const BINARY_SAMPLE_BYTES: usize = 8192;

/// Non-strict `realpath`: unlike `fs::canonicalize`, a missing tail is not an
/// error, so a nonexistent store path reports NOT_FOUND rather than INVALID_PATH.
fn realpath(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if let Ok(real) = fs::canonicalize(&resolved) {
                    resolved = real;
                }
            }
            _ => {}
        }
    }
    resolved
}

/// Validate that a path is within `/nix/store/` and does not escape it.
pub fn validate_store_path(path: &str) -> bool {
    // A string prefix on purpose: `Path::starts_with` would admit bare /nix/store.
    !path.contains('\0') && realpath(Path::new(path)).to_string_lossy().starts_with("/nix/store/")
}

fn validate_query(query: &str) -> Result<&str, String> {
    let path = crate::text::strip(query);
    if path.is_empty() {
        return Err(error("Store path required (e.g., '/nix/store/<hash>-<name>')", "INVALID_PATH"));
    }
    if !path.starts_with('/') {
        return Err(error(format!("Absolute store path required, got {}", py_repr(query)), "INVALID_PATH"));
    }
    if !validate_store_path(path) {
        return Err(error(
            format!("Invalid store path: must stay within /nix/store/, got {}", py_repr(query)),
            "INVALID_PATH",
        ));
    }
    Ok(path)
}

fn io_error(err: &io::Error, subject: &str, prefix: &str) -> String {
    match err.kind() {
        ErrorKind::PermissionDenied => error(format!("Permission denied: {subject}"), "PERMISSION_ERROR"),
        _ => error(format!("{prefix}: {err}"), "OS_ERROR"),
    }
}

/// A directory listing: sorted (dirs, files with sizes). A per-entry stat
/// failure keeps the entry with no size.
pub struct Listing {
    pub dirs: Vec<String>,
    pub files: Vec<(String, Option<u64>)>,
}

impl Listing {
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.files.is_empty()
    }

    pub fn file_line(name: &str, size: Option<u64>) -> String {
        match size {
            Some(size) => format!("  {name} ({})", format_size(size)),
            None => format!("  {name}"),
        }
    }
}

pub fn scan_directory(path: &Path) -> io::Result<Listing> {
    let mut listing = Listing { dirs: Vec::new(), files: Vec::new() };
    for entry in fs::read_dir(path)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match fs::metadata(entry.path()) {
            Ok(meta) if meta.is_dir() => listing.dirs.push(name),
            Ok(meta) => listing.files.push((name, Some(meta.len()))),
            Err(_) => listing.files.push((name, None)),
        }
    }
    listing.dirs.sort();
    listing.files.sort();
    Ok(listing)
}

/// List the contents of a directory inside `/nix/store`. `limit` caps the
/// entries printed, dirs first.
pub fn store_ls(query: &str, limit: usize) -> String {
    let path = match validate_query(query) {
        Ok(path) => path,
        Err(message) => return message,
    };
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return error(format!("Path not found: {path}"), "NOT_FOUND"),
    };
    if !meta.is_dir() {
        return error(format!("Not a directory: {path}"), "NOT_DIRECTORY");
    }
    let listing = match scan_directory(Path::new(path)) {
        Ok(listing) => listing,
        Err(err) => return io_error(&err, path, "Cannot list directory"),
    };
    if listing.is_empty() {
        return format!("Directory '{path}' is empty.");
    }
    let (total_dirs, total_files) = (listing.dirs.len(), listing.files.len());
    let total = total_dirs + total_files;
    let shown_dirs = &listing.dirs[..limit.min(total_dirs)];
    let shown_files = &listing.files[..(limit - shown_dirs.len()).min(total_files)];
    let shown_total = shown_dirs.len() + shown_files.len();

    let mut header = format!("Contents of {path} ({total_dirs} dirs, {total_files} files):");
    if shown_total < total {
        header.push_str(&format!(" showing {shown_total} of {total}"));
    }
    let mut lines = vec![header, String::new()];
    lines.extend(shown_dirs.iter().map(|name| format!("  {name}/")));
    lines.extend(shown_files.iter().map(|(name, size)| Listing::file_line(name, *size)));
    lines.join("\n")
}

/// Why a text file could not be read.
pub enum ReadError {
    TooLarge(u64),
    Binary(u64),
    Access(io::Error),
    Read(io::Error),
}

/// A text file's size and decoded body. Size, sniff and read all use one
/// descriptor, so nothing can swap the file between the checks.
pub fn read_text(path: &Path) -> Result<(u64, String), ReadError> {
    let mut file = File::open(path).map_err(ReadError::Access)?;
    let size = file.metadata().map_err(ReadError::Access)?.len();
    if size > MAX_FILE_SIZE {
        return Err(ReadError::TooLarge(size));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.by_ref().take(MAX_FILE_SIZE).read_to_end(&mut bytes).map_err(ReadError::Read)?;
    if bytes[..bytes.len().min(BINARY_SAMPLE_BYTES)].contains(&0) {
        return Err(ReadError::Binary(size));
    }
    // Text-mode semantics: universal newlines, undecodable bytes replaced.
    Ok((size, String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n")))
}

/// `(shown lines, total lines)` for a decoded file.
pub fn limited_lines(text: &str, limit: usize) -> (Vec<String>, usize) {
    if text.is_empty() {
        return (Vec::new(), 0);
    }
    let all: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).split('\n').collect();
    (all.iter().take(limit).map(|line| (*line).to_owned()).collect(), all.len())
}

/// The shared `File:`/`Size:` rendering of a read.
pub fn render_read(label: &str, size: u64, text: &str, limit: usize) -> String {
    let (lines, total) = limited_lines(text, limit);
    let mut out = vec![format!("File: {label}"), format!("Size: {}", format_size(size)), String::new()];
    if total > limit {
        out.push(format!("(Showing {limit} of {total} lines)"));
        out.push(String::new());
    }
    out.extend(lines);
    out.join("\n")
}

pub fn too_large(size: u64) -> String {
    error(format!("File too large: {} (max {})", format_size(size), format_size(MAX_FILE_SIZE)), "FILE_TOO_LARGE")
}

/// Read a text file inside `/nix/store` with a line limit.
pub fn store_read(query: &str, limit: usize) -> String {
    let path = match validate_query(query) {
        Ok(path) => path,
        Err(message) => return message,
    };
    let target = Path::new(path);
    if !target.exists() {
        return error(format!("File not found: {path}"), "NOT_FOUND");
    }
    if target.is_dir() {
        return error(format!("'{path}' is a directory. Use type='ls' to list contents."), "IS_DIRECTORY");
    }
    match read_text(target) {
        Ok((size, text)) => render_read(path, size, &text, limit),
        Err(ReadError::TooLarge(size)) => too_large(size),
        Err(ReadError::Binary(size)) => {
            error(format!("Binary file detected: {path} ({})", format_size(size)), "BINARY_FILE")
        }
        Err(ReadError::Access(err)) => io_error(&err, path, "Cannot access file"),
        Err(ReadError::Read(err)) => io_error(&err, path, "Cannot read file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_store_and_escapes_are_invalid() {
        assert!(!validate_store_path("/nix/store"));
        assert!(!validate_store_path("/nix/store/../../etc/passwd"));
        assert!(!validate_store_path("/etc/passwd"));
        assert!(!validate_store_path("/nix/store/a\0b"));
        assert!(validate_store_path("/nix/store/doesnotexist"));
    }

    #[test]
    fn query_errors_echo_repr() {
        assert_eq!(store_ls("", 5), "Error (INVALID_PATH): Store path required (e.g., '/nix/store/<hash>-<name>')");
        assert_eq!(store_ls("rel", 5), "Error (INVALID_PATH): Absolute store path required, got 'rel'");
        assert_eq!(store_read("/nix/store/missing-x", 5), "Error (NOT_FOUND): File not found: /nix/store/missing-x");
    }

    #[test]
    fn line_limits_follow_text_mode_counting() {
        assert_eq!(limited_lines("", 5), (vec![], 0));
        assert_eq!(limited_lines("a\n\n", 1), (vec!["a".to_owned()], 2));
        assert_eq!(limited_lines("a\nb", 5).1, 2);
    }

    #[test]
    fn crlf_and_invalid_utf8_decode_like_text_mode() {
        let dir = std::env::temp_dir().join(format!("mcp-nixos-store-{}", std::process::id()));
        fs::create_dir_all(&dir).ok();
        let file = dir.join("mixed.txt");
        fs::write(&file, b"a\r\nb\rc\xff\n").ok();
        let (size, text) = read_text(&file).ok().unwrap_or_default();
        assert_eq!((size, text.as_str()), (8, "a\nb\nc\u{fffd}\n"));

        fs::write(&file, b"\x00binary").ok();
        assert!(matches!(read_text(&file), Err(ReadError::Binary(7))));
        fs::write(&file, vec![b'x'; MAX_FILE_SIZE as usize + 1]).ok();
        assert!(matches!(read_text(&file), Err(ReadError::TooLarge(_))));
        fs::remove_dir_all(&dir).ok();
    }
}
