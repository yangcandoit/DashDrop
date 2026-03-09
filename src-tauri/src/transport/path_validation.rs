use std::path::{Component, Path, PathBuf};
use anyhow::Result;

/// Windows reserved device names (case-insensitive).
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate a relative path from a FileItem.rel_path per PROTOCOL.md §5.1.
/// Returns the safe absolute destination path on success.
pub fn validate_rel_path(rel_path: &str, save_root: &Path) -> Result<PathBuf, PathError> {
    // Rule 1: reject absolute paths
    if rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return Err(PathError::AbsolutePath);
    }
    // Windows-style absolute: C:\...
    if rel_path.len() >= 3 {
        let bytes = rel_path.as_bytes();
        if bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
            return Err(PathError::AbsolutePath);
        }
    }

    // Rule 2: reject leading/trailing separators and double separators
    if rel_path.starts_with('/') || rel_path.starts_with('\\')
        || rel_path.ends_with('/') || rel_path.ends_with('\\')
        || rel_path.contains("//") || rel_path.contains("\\\\")
        || rel_path.contains("/\\") || rel_path.contains("\\/")
    {
        return Err(PathError::EmptySegment);
    }

    // Check each path segment
    let normalized = rel_path.replace('\\', "/");
    for segment in normalized.split('/') {
        validate_segment(segment)?;
    }

    // Build the candidate path
    let candidate = save_root.join(&normalized);

    // Normalize/canonicalize what we can without requiring the path to exist
    let normalized_candidate = normalize_path(&candidate);

    // Rule 6: prefix assertion — final path must be under save_root
    let normalized_root = normalize_path(save_root);
    if !normalized_candidate.starts_with(&normalized_root) {
        return Err(PathError::PathTraversal);
    }

    Ok(normalized_candidate)
}

fn validate_segment(segment: &str) -> Result<(), PathError> {
    // Rule 2: reject ..
    if segment == ".." {
        return Err(PathError::PathTraversal);
    }
    // Rule 3: reject empty segments
    if segment.is_empty() {
        return Err(PathError::EmptySegment);
    }
    // Rule 4: reject Windows reserved names
    let upper = segment.to_uppercase();
    // Strip extension for comparison (e.g., "NUL.txt" is also forbidden)
    let base = upper.split('.').next().unwrap_or(&upper);
    if WINDOWS_RESERVED.contains(&base) {
        return Err(PathError::WindowsReserved);
    }
    Ok(())
}

/// Normalize a path without requiring it to exist.
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => result.push(p.as_os_str()),
            Component::RootDir => result.push("/"),
            Component::CurDir => {} // skip .
            Component::ParentDir => { result.pop(); } // handle ..
            Component::Normal(c) => result.push(c),
        }
    }
    result
}

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("absolute paths are not allowed")]
    AbsolutePath,
    #[error("path traversal (.. segment) is not allowed")]
    PathTraversal,
    #[error("empty path segments are not allowed")]
    EmptySegment,
    #[error("Windows reserved device name")]
    WindowsReserved,
}

impl From<PathError> for crate::transport::protocol::ErrorCode {
    fn from(_: PathError) -> Self {
        crate::transport::protocol::ErrorCode::InvalidPath
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("/tmp/dashdrop/test-transfer")
    }

    #[test]
    fn valid_simple_file() {
        assert!(validate_rel_path("hello.txt", &root()).is_ok());
    }

    #[test]
    fn valid_nested() {
        assert!(validate_rel_path("docs/readme.md", &root()).is_ok());
    }

    #[test]
    fn reject_absolute_unix() {
        assert!(validate_rel_path("/etc/passwd", &root()).is_err());
    }

    #[test]
    fn reject_traversal() {
        assert!(validate_rel_path("../secret", &root()).is_err());
    }

    #[test]
    fn reject_nested_traversal() {
        assert!(validate_rel_path("a/b/../../../../../../etc/passwd", &root()).is_err());
    }

    #[test]
    fn reject_double_slash() {
        assert!(validate_rel_path("a//b", &root()).is_err());
    }

    #[test]
    fn reject_windows_reserved() {
        assert!(validate_rel_path("CON", &root()).is_err());
        assert!(validate_rel_path("nul.txt", &root()).is_err());
        assert!(validate_rel_path("COM1", &root()).is_err());
    }
}
