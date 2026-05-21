// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

/// Maximum byte length of a normalized VFS path, matching the inline path
/// limit in the wire protocol (`VFS_OPENAT_INLINE_PATH_MAX` /
/// `VFS_STATX_INLINE_PATH_MAX` in `yarm_ipc_abi::vfs_abi`).
pub const VFS_INLINE_PATH_MAX: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// Input is zero bytes.
    Empty,
    /// Input does not start with `/`.
    NotAbsolute,
    /// Normalized output would exceed `VFS_INLINE_PATH_MAX` bytes.
    TooLong,
}

impl PathError {
    pub const fn as_str(self) -> &'static str {
        match self {
            PathError::Empty => "empty",
            PathError::NotAbsolute => "not-absolute",
            PathError::TooLong => "too-long",
        }
    }
}

/// Stack-allocated buffer holding a normalized absolute path.
#[derive(Debug)]
pub struct NormalizedPath {
    buf: [u8; VFS_INLINE_PATH_MAX],
    len: usize,
}

impl NormalizedPath {
    /// Returns the normalized path bytes, always starting with `/`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

/// Normalize an absolute POSIX-style path into a stack-allocated buffer.
///
/// Rules applied in order:
/// - Empty input → `PathError::Empty`
/// - Non-`/`-prefixed input → `PathError::NotAbsolute`
/// - Repeated `/` (e.g. `//`) → collapsed to a single `/`
/// - `.` components → removed
/// - `..` components → parent segment popped; `..` at root stays at root
/// - Trailing slash → removed (except when the entire path is `/`)
/// - Normalized result > `VFS_INLINE_PATH_MAX` bytes → `PathError::TooLong`
pub fn normalize(path: &[u8]) -> Result<NormalizedPath, PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path[0] != b'/' {
        return Err(PathError::NotAbsolute);
    }

    let mut buf = [0u8; VFS_INLINE_PATH_MAX];
    buf[0] = b'/';
    let mut out_len = 1usize;

    // Iterate over the path after the leading `/`, splitting on `/`.
    let rest = &path[1..];
    let mut i = 0usize;
    while i <= rest.len() {
        // Find the next component (up to the next `/` or end of input).
        let start = i;
        while i < rest.len() && rest[i] != b'/' {
            i += 1;
        }
        let component = &rest[start..i];
        i += 1; // step past `/` separator (or harmlessly past end of slice)

        match component {
            b"" | b"." => {
                // empty (from `//` or trailing `/`) and `.` are both no-ops
            }
            b".." => {
                // Pop the last path segment; never go above root.
                if out_len > 1 {
                    // Walk back to the preceding `/`.
                    let mut j = out_len - 1;
                    while j > 0 && buf[j] != b'/' {
                        j -= 1;
                    }
                    // j == 0 means the only `/` is the root slash.
                    out_len = if j == 0 { 1 } else { j };
                }
                // out_len == 1 → already at root, no-op
            }
            _ => {
                // Regular component: append (separator +) component.
                let need_sep = out_len > 1;
                let append = component.len() + need_sep as usize;
                if out_len + append > VFS_INLINE_PATH_MAX {
                    return Err(PathError::TooLong);
                }
                if need_sep {
                    buf[out_len] = b'/';
                    out_len += 1;
                }
                buf[out_len..out_len + component.len()].copy_from_slice(component);
                out_len += component.len();
            }
        }
    }

    Ok(NormalizedPath { buf, len: out_len })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(input: &[u8]) -> Result<alloc::vec::Vec<u8>, PathError> {
        normalize(input).map(|n| n.as_bytes().to_vec())
    }

    // ── rejection cases ─────────────────────────────────────────────────────

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(normalize(b"").unwrap_err(), PathError::Empty);
    }

    #[test]
    fn relative_path_is_rejected() {
        assert_eq!(normalize(b"foo/bar").unwrap_err(), PathError::NotAbsolute);
        assert_eq!(normalize(b"./foo").unwrap_err(), PathError::NotAbsolute);
        assert_eq!(normalize(b"../foo").unwrap_err(), PathError::NotAbsolute);
    }

    #[test]
    fn path_longer_than_96_bytes_after_normalization_is_rejected() {
        // 97 bytes of `a` after the leading slash = 98 total
        let mut long = alloc::vec![b'/'];
        long.extend_from_slice(&[b'a'; 96]);
        assert_eq!(normalize(&long).unwrap_err(), PathError::TooLong);
    }

    // ── root handling ────────────────────────────────────────────────────────

    #[test]
    fn root_path_normalizes_to_slash() {
        assert_eq!(norm(b"/").unwrap(), b"/");
    }

    #[test]
    fn multiple_slashes_at_root_collapse_to_slash() {
        assert_eq!(norm(b"//").unwrap(), b"/");
        assert_eq!(norm(b"///").unwrap(), b"/");
    }

    // ── dot removal ──────────────────────────────────────────────────────────

    #[test]
    fn single_dot_components_are_removed() {
        assert_eq!(norm(b"/./").unwrap(), b"/");
        assert_eq!(norm(b"/foo/./bar").unwrap(), b"/foo/bar");
        assert_eq!(norm(b"/./foo").unwrap(), b"/foo");
        assert_eq!(norm(b"/foo/.").unwrap(), b"/foo");
    }

    // ── double-slash collapsing ──────────────────────────────────────────────

    #[test]
    fn repeated_slashes_are_collapsed() {
        assert_eq!(norm(b"//foo//bar//").unwrap(), b"/foo/bar");
        assert_eq!(norm(b"/foo///bar").unwrap(), b"/foo/bar");
    }

    // ── double-dot resolution ────────────────────────────────────────────────

    #[test]
    fn double_dot_resolves_parent() {
        assert_eq!(norm(b"/foo/../bar").unwrap(), b"/bar");
        assert_eq!(norm(b"/foo/bar/..").unwrap(), b"/foo");
        assert_eq!(norm(b"/foo/bar/../..").unwrap(), b"/");
        assert_eq!(norm(b"/a/b/c/../../d").unwrap(), b"/a/d");
    }

    #[test]
    fn double_dot_at_root_stays_at_root() {
        assert_eq!(norm(b"/..").unwrap(), b"/");
        assert_eq!(norm(b"/../foo").unwrap(), b"/foo");
        assert_eq!(norm(b"/../../foo").unwrap(), b"/foo");
        assert_eq!(norm(b"/../..").unwrap(), b"/");
    }

    // ── trailing slash removal ───────────────────────────────────────────────

    #[test]
    fn trailing_slash_is_removed() {
        assert_eq!(norm(b"/foo/").unwrap(), b"/foo");
        assert_eq!(norm(b"/foo/bar/").unwrap(), b"/foo/bar");
    }

    // ── combined cases ───────────────────────────────────────────────────────

    #[test]
    fn canonical_mount_paths_are_unchanged() {
        assert_eq!(norm(b"/initramfs/boot-marker").unwrap(), b"/initramfs/boot-marker");
        assert_eq!(norm(b"/dev/null").unwrap(), b"/dev/null");
        assert_eq!(norm(b"/dev/console").unwrap(), b"/dev/console");
    }

    #[test]
    fn complex_combined_normalization() {
        assert_eq!(norm(b"/foo/./bar/../baz").unwrap(), b"/foo/baz");
        assert_eq!(norm(b"//foo//./bar//../baz//").unwrap(), b"/foo/baz");
        assert_eq!(norm(b"/a/b/./c/../d/../../e").unwrap(), b"/a/e");
    }

    #[test]
    fn exactly_96_byte_normalized_result_is_accepted() {
        // Construct a path that normalizes to exactly 96 bytes:
        // "/" + "a" * 95 = 96 bytes total.
        let mut input = alloc::vec![b'/'];
        input.extend_from_slice(&[b'a'; 95]);
        let result = norm(&input).unwrap();
        assert_eq!(result.len(), 96);
        assert_eq!(&result[..], &input[..]);
    }

    #[test]
    fn path_error_as_str_coverage() {
        assert_eq!(PathError::Empty.as_str(), "empty");
        assert_eq!(PathError::NotAbsolute.as_str(), "not-absolute");
        assert_eq!(PathError::TooLong.as_str(), "too-long");
    }
}
