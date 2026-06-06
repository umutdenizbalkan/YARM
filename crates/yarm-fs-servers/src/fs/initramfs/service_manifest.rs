// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const SERVICE_MANIFEST_MAX_BYTES: usize = 8192;
pub const SERVICE_MANIFEST_MAX_LINE_BYTES: usize = 256;
pub const SERVICE_MANIFEST_MAX_PATH_BYTES: usize = 255;
pub const SERVICE_MANIFEST_MAX_ENTRIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceManifestEntry {
    path: [u8; SERVICE_MANIFEST_MAX_PATH_BYTES],
    path_len: u16,
    source_line: u16,
}

impl ServiceManifestEntry {
    const EMPTY: Self = Self {
        path: [0; SERVICE_MANIFEST_MAX_PATH_BYTES],
        path_len: 0,
        source_line: 0,
    };

    fn new(path: &[u8], source_line: usize) -> Self {
        let mut entry = Self::EMPTY;
        entry.path[..path.len()].copy_from_slice(path);
        entry.path_len = path.len() as u16;
        entry.source_line = source_line as u16;
        entry
    }

    pub fn path(&self) -> &[u8] {
        &self.path[..self.path_len as usize]
    }

    pub const fn source_line(&self) -> usize {
        self.source_line as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceManifest {
    entries: [ServiceManifestEntry; SERVICE_MANIFEST_MAX_ENTRIES],
    entry_count: u8,
}

impl ServiceManifest {
    const fn empty() -> Self {
        Self {
            entries: [ServiceManifestEntry::EMPTY; SERVICE_MANIFEST_MAX_ENTRIES],
            entry_count: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.entry_count as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    pub fn entries(&self) -> &[ServiceManifestEntry] {
        &self.entries[..self.len()]
    }

    fn contains_path(&self, path: &[u8]) -> bool {
        self.entries().iter().any(|entry| entry.path() == path)
    }

    fn push(&mut self, path: &[u8], source_line: usize) -> Result<(), ServiceManifestError> {
        if self.len() >= SERVICE_MANIFEST_MAX_ENTRIES {
            return Err(ServiceManifestError::TooManyEntries { line: source_line });
        }
        let index = self.len();
        self.entries[index] = ServiceManifestEntry::new(path, source_line);
        self.entry_count += 1;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManifestError {
    Empty,
    TooLarge,
    InvalidUtf8,
    LineTooLong { line: usize },
    TooManyEntries { line: usize },
    RelativePath { line: usize },
    InvalidPath { line: usize },
    ParentComponent { line: usize },
    ContainsWhitespace { line: usize },
    DuplicatePath { line: usize },
    UnsupportedInlineComment { line: usize },
}

/// Parses the helper-only v1 service-list format.
///
/// The parser is fail-whole-file: any invalid non-comment line rejects the
/// complete input. It performs no archive lookup and has no runtime spawn side
/// effects.
pub fn parse_service_manifest(bytes: &[u8]) -> Result<ServiceManifest, ServiceManifestError> {
    if bytes.len() > SERVICE_MANIFEST_MAX_BYTES {
        return Err(ServiceManifestError::TooLarge);
    }
    let text = core::str::from_utf8(bytes).map_err(|_| ServiceManifestError::InvalidUtf8)?;
    let mut manifest = ServiceManifest::empty();

    for (index, raw_line) in text.split('\n').enumerate() {
        let line_number = index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.len() > SERVICE_MANIFEST_MAX_LINE_BYTES {
            return Err(ServiceManifestError::LineTooLong { line: line_number });
        }
        if line.is_empty() || line.chars().all(char::is_whitespace) {
            continue;
        }
        if line
            .trim_start_matches(char::is_whitespace)
            .starts_with('#')
        {
            continue;
        }
        if line.contains('#') {
            return Err(ServiceManifestError::UnsupportedInlineComment { line: line_number });
        }
        if line.chars().any(|character| character.is_whitespace()) {
            return Err(ServiceManifestError::ContainsWhitespace { line: line_number });
        }
        if !line.starts_with('/') {
            return Err(ServiceManifestError::RelativePath { line: line_number });
        }
        if line == "/" || line.len() > SERVICE_MANIFEST_MAX_PATH_BYTES {
            return Err(ServiceManifestError::InvalidPath { line: line_number });
        }

        let mut components = line.split('/');
        if components.next() != Some("") {
            return Err(ServiceManifestError::RelativePath { line: line_number });
        }
        for component in components {
            if component == ".." {
                return Err(ServiceManifestError::ParentComponent { line: line_number });
            }
            if component.is_empty() || component == "." || component.chars().any(char::is_control) {
                return Err(ServiceManifestError::InvalidPath { line: line_number });
            }
        }

        let path = line.as_bytes();
        if manifest.contains_path(path) {
            return Err(ServiceManifestError::DuplicatePath { line: line_number });
        }
        manifest.push(path, line_number)?;
    }

    if manifest.is_empty() {
        return Err(ServiceManifestError::Empty);
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, string::String, vec, vec::Vec};

    fn paths(manifest: &ServiceManifest) -> Vec<&[u8]> {
        manifest
            .entries()
            .iter()
            .map(ServiceManifestEntry::path)
            .collect()
    }

    #[test]
    fn parses_one_service() {
        let manifest = parse_service_manifest(b"/sbin/initramfs_srv\n").expect("manifest");
        assert_eq!(paths(&manifest), [b"/sbin/initramfs_srv".as_slice()]);
        assert_eq!(manifest.entries()[0].source_line(), 1);
    }

    #[test]
    fn parses_multiple_services_blank_lines_and_full_line_comments() {
        let manifest = parse_service_manifest(
            b"# core\n\n  # storage follows\r\n/sbin/devfs_srv\n/sbin/vfs_srv\r\n",
        )
        .expect("manifest");
        assert_eq!(
            paths(&manifest),
            [b"/sbin/devfs_srv".as_slice(), b"/sbin/vfs_srv".as_slice()]
        );
        assert_eq!(manifest.entries()[0].source_line(), 4);
        assert_eq!(manifest.entries()[1].source_line(), 5);
    }

    #[test]
    fn rejects_empty_and_comments_only_manifests() {
        assert_eq!(
            parse_service_manifest(b""),
            Err(ServiceManifestError::Empty)
        );
        assert_eq!(
            parse_service_manifest(b"# only comments\n  # still comments\n\n"),
            Err(ServiceManifestError::Empty)
        );
    }

    #[test]
    fn rejects_relative_parent_whitespace_root_and_inline_comment_paths() {
        assert_eq!(
            parse_service_manifest(b"sbin/devfs_srv\n"),
            Err(ServiceManifestError::RelativePath { line: 1 })
        );
        assert_eq!(
            parse_service_manifest(b"/sbin/../evil\n"),
            Err(ServiceManifestError::ParentComponent { line: 1 })
        );
        assert_eq!(
            parse_service_manifest(b"/sbin/foo bar\n"),
            Err(ServiceManifestError::ContainsWhitespace { line: 1 })
        );
        assert_eq!(
            parse_service_manifest(b"/\n"),
            Err(ServiceManifestError::InvalidPath { line: 1 })
        );
        assert_eq!(
            parse_service_manifest(b"/sbin/devfs_srv # inline\n"),
            Err(ServiceManifestError::UnsupportedInlineComment { line: 1 })
        );
    }

    #[test]
    fn rejects_too_long_line_and_manifest() {
        let line = format!("/{}", "x".repeat(SERVICE_MANIFEST_MAX_LINE_BYTES));
        assert_eq!(
            parse_service_manifest(line.as_bytes()),
            Err(ServiceManifestError::LineTooLong { line: 1 })
        );
        let bytes = vec![b'\n'; SERVICE_MANIFEST_MAX_BYTES + 1];
        assert_eq!(
            parse_service_manifest(&bytes),
            Err(ServiceManifestError::TooLarge)
        );
    }

    #[test]
    fn rejects_too_many_entries() {
        let mut text = String::new();
        for index in 0..=SERVICE_MANIFEST_MAX_ENTRIES {
            text.push_str(&format!("/sbin/service{index}\n"));
        }
        assert_eq!(
            parse_service_manifest(text.as_bytes()),
            Err(ServiceManifestError::TooManyEntries {
                line: SERVICE_MANIFEST_MAX_ENTRIES + 1
            })
        );
    }

    #[test]
    fn rejects_duplicate_paths() {
        assert_eq!(
            parse_service_manifest(b"/sbin/vfs_srv\n/sbin/vfs_srv\n"),
            Err(ServiceManifestError::DuplicatePath { line: 2 })
        );
    }

    #[test]
    fn invalid_line_rejects_whole_manifest() {
        assert_eq!(
            parse_service_manifest(b"/sbin/devfs_srv\nsbin/bad\n/sbin/vfs_srv\n"),
            Err(ServiceManifestError::RelativePath { line: 2 })
        );
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert_eq!(
            parse_service_manifest(b"/sbin/devfs_srv\n\xff\n"),
            Err(ServiceManifestError::InvalidUtf8)
        );
    }

    #[test]
    fn accepts_maximum_path_length_and_rejects_one_more() {
        let accepted = format!("/{}", "x".repeat(SERVICE_MANIFEST_MAX_PATH_BYTES - 1));
        let manifest = parse_service_manifest(accepted.as_bytes()).expect("maximum path");
        assert_eq!(
            manifest.entries()[0].path().len(),
            SERVICE_MANIFEST_MAX_PATH_BYTES
        );

        let rejected = format!("/{}", "x".repeat(SERVICE_MANIFEST_MAX_PATH_BYTES));
        assert_eq!(
            parse_service_manifest(rejected.as_bytes()),
            Err(ServiceManifestError::InvalidPath { line: 1 })
        );
    }

    #[test]
    fn rejects_empty_dot_repeated_and_control_components() {
        for input in [
            b"/sbin//devfs_srv\n".as_slice(),
            b"/sbin/./devfs_srv\n".as_slice(),
            b"/sbin/devfs\x01srv\n".as_slice(),
        ] {
            assert_eq!(
                parse_service_manifest(input),
                Err(ServiceManifestError::InvalidPath { line: 1 })
            );
        }
    }
}
