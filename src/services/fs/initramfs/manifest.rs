// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::archive::{
    INITRAMFS_INIT_PATH_PTR, INITRAMFS_PROC_MGR_PATH_PTR, INITRAMFS_SUPERVISOR_PATH_PTR,
    INITRAMFS_VFS_PATH_PTR,
};

const MANIFEST_MAGIC: u32 = 0x5941_524D;
const MANIFEST_VERSION_V1: u16 = 1;
const MANIFEST_HEADER_BYTES: usize = 8;
const MANIFEST_ENTRY_BYTES: usize = 28;
const MANIFEST_EXPECTED_ENTRIES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestEntryWire {
    pub path_ptr: u64,
    pub file_len: u64,
    pub entry_addr: u64,
    pub abi: u16,
    pub flags: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreServiceImageManifest {
    pub init: ManifestEntryWire,
    pub process_manager: ManifestEntryWire,
    pub vfs: ManifestEntryWire,
    pub supervisor: ManifestEntryWire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitramfsManifestError {
    Truncated,
    BadMagic,
    UnsupportedVersion,
    UnexpectedEntryCount,
    DuplicatePath,
    MissingInit,
    MissingProcessManager,
    MissingVfs,
    MissingSupervisor,
    ZeroLengthImage,
    ZeroEntryAddress,
}

pub fn parse_core_service_manifest(
    bytes: &[u8],
) -> Result<CoreServiceImageManifest, InitramfsManifestError> {
    if bytes.len() < MANIFEST_HEADER_BYTES {
        return Err(InitramfsManifestError::Truncated);
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().expect("magic"));
    if magic != MANIFEST_MAGIC {
        return Err(InitramfsManifestError::BadMagic);
    }
    let version = u16::from_le_bytes(bytes[4..6].try_into().expect("version"));
    if version != MANIFEST_VERSION_V1 {
        return Err(InitramfsManifestError::UnsupportedVersion);
    }
    let entry_count = u16::from_le_bytes(bytes[6..8].try_into().expect("count")) as usize;
    if entry_count != MANIFEST_EXPECTED_ENTRIES {
        return Err(InitramfsManifestError::UnexpectedEntryCount);
    }

    let required = MANIFEST_HEADER_BYTES + entry_count.saturating_mul(MANIFEST_ENTRY_BYTES);
    if bytes.len() < required {
        return Err(InitramfsManifestError::Truncated);
    }

    let mut init = None;
    let mut process_manager = None;
    let mut vfs = None;
    let mut supervisor = None;

    for index in 0..entry_count {
        let base = MANIFEST_HEADER_BYTES + index * MANIFEST_ENTRY_BYTES;
        let entry = ManifestEntryWire {
            path_ptr: u64::from_le_bytes(bytes[base..base + 8].try_into().expect("path")),
            file_len: u64::from_le_bytes(bytes[base + 8..base + 16].try_into().expect("len")),
            entry_addr: u64::from_le_bytes(bytes[base + 16..base + 24].try_into().expect("entry")),
            abi: u16::from_le_bytes(bytes[base + 24..base + 26].try_into().expect("abi")),
            flags: u16::from_le_bytes(bytes[base + 26..base + 28].try_into().expect("flags")),
        };

        if entry.file_len == 0 {
            return Err(InitramfsManifestError::ZeroLengthImage);
        }
        if entry.entry_addr == 0 {
            return Err(InitramfsManifestError::ZeroEntryAddress);
        }

        let slot = match entry.path_ptr {
            INITRAMFS_INIT_PATH_PTR => &mut init,
            INITRAMFS_PROC_MGR_PATH_PTR => &mut process_manager,
            INITRAMFS_VFS_PATH_PTR => &mut vfs,
            INITRAMFS_SUPERVISOR_PATH_PTR => &mut supervisor,
            _ => return Err(InitramfsManifestError::DuplicatePath),
        };
        if slot.is_some() {
            return Err(InitramfsManifestError::DuplicatePath);
        }
        *slot = Some(entry);
    }

    Ok(CoreServiceImageManifest {
        init: init.ok_or(InitramfsManifestError::MissingInit)?,
        process_manager: process_manager.ok_or(InitramfsManifestError::MissingProcessManager)?,
        vfs: vfs.ok_or(InitramfsManifestError::MissingVfs)?,
        supervisor: supervisor.ok_or(InitramfsManifestError::MissingSupervisor)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::std::vec::Vec;

    fn encode_manifest(entries: &[ManifestEntryWire]) -> Vec<u8> {
        let mut out = Vec::with_capacity(MANIFEST_HEADER_BYTES + entries.len() * MANIFEST_ENTRY_BYTES);
        out.extend_from_slice(&MANIFEST_MAGIC.to_le_bytes());
        out.extend_from_slice(&MANIFEST_VERSION_V1.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for entry in entries {
            out.extend_from_slice(&entry.path_ptr.to_le_bytes());
            out.extend_from_slice(&entry.file_len.to_le_bytes());
            out.extend_from_slice(&entry.entry_addr.to_le_bytes());
            out.extend_from_slice(&entry.abi.to_le_bytes());
            out.extend_from_slice(&entry.flags.to_le_bytes());
        }
        out
    }

    fn baseline_entries() -> [ManifestEntryWire; 4] {
        [
            ManifestEntryWire {
                path_ptr: INITRAMFS_INIT_PATH_PTR,
                file_len: 2048,
                entry_addr: 0x401000,
                abi: 1,
                flags: 0,
            },
            ManifestEntryWire {
                path_ptr: INITRAMFS_PROC_MGR_PATH_PTR,
                file_len: 1536,
                entry_addr: 0x402000,
                abi: 1,
                flags: 0,
            },
            ManifestEntryWire {
                path_ptr: INITRAMFS_VFS_PATH_PTR,
                file_len: 1536,
                entry_addr: 0x403000,
                abi: 1,
                flags: 0,
            },
            ManifestEntryWire {
                path_ptr: INITRAMFS_SUPERVISOR_PATH_PTR,
                file_len: 1536,
                entry_addr: 0x404000,
                abi: 1,
                flags: 0,
            },
        ]
    }

    #[test]
    fn core_service_manifest_roundtrips_required_entries() {
        let bytes = encode_manifest(&baseline_entries());
        let manifest = parse_core_service_manifest(&bytes).expect("manifest");
        assert_eq!(manifest.init.path_ptr, INITRAMFS_INIT_PATH_PTR);
        assert_eq!(manifest.process_manager.path_ptr, INITRAMFS_PROC_MGR_PATH_PTR);
        assert_eq!(manifest.vfs.path_ptr, INITRAMFS_VFS_PATH_PTR);
        assert_eq!(manifest.supervisor.path_ptr, INITRAMFS_SUPERVISOR_PATH_PTR);
    }

    #[test]
    fn core_service_manifest_rejects_missing_required_entry() {
        let mut entries = baseline_entries();
        entries[3].path_ptr = INITRAMFS_VFS_PATH_PTR;
        let bytes = encode_manifest(&entries);
        assert_eq!(
            parse_core_service_manifest(&bytes),
            Err(InitramfsManifestError::DuplicatePath)
        );
    }

    #[test]
    fn core_service_manifest_rejects_corrupt_zero_entry_or_length() {
        let mut entries = baseline_entries();
        entries[1].entry_addr = 0;
        let bytes = encode_manifest(&entries);
        assert_eq!(
            parse_core_service_manifest(&bytes),
            Err(InitramfsManifestError::ZeroEntryAddress)
        );

        let mut entries = baseline_entries();
        entries[1].file_len = 0;
        let bytes = encode_manifest(&entries);
        assert_eq!(
            parse_core_service_manifest(&bytes),
            Err(InitramfsManifestError::ZeroLengthImage)
        );
    }
}
