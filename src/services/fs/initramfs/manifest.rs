// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::archive::{
    INITRAMFS_INIT_PATH_PTR, INITRAMFS_PROC_MGR_PATH_PTR, INITRAMFS_SUPERVISOR_PATH_PTR,
    INITRAMFS_VFS_PATH_PTR,
};
use crate::kernel::process::ElfImageInfo;

const MANIFEST_MAGIC: u32 = 0x5941_524D;
const MANIFEST_VERSION_V1: u16 = 1;
const MANIFEST_HEADER_BYTES: usize = 8;
const MANIFEST_ENTRY_BYTES: usize = 28;
const MANIFEST_EXPECTED_ENTRIES: usize = 4;
const MAX_LOAD_SEGMENTS: usize = 8;

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
pub struct ElfLoadSegmentPlan {
    pub file_offset: u64,
    pub virt_addr: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceElfLaunchPlan {
    pub manifest: ManifestEntryWire,
    pub validated_entry: u64,
    pub load_segment_count: usize,
    pub load_segments: [Option<ElfLoadSegmentPlan>; MAX_LOAD_SEGMENTS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreServiceElfLaunchPlan {
    pub init: ServiceElfLaunchPlan,
    pub process_manager: ServiceElfLaunchPlan,
    pub vfs: ServiceElfLaunchPlan,
    pub supervisor: ServiceElfLaunchPlan,
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
    MissingImagePayload,
    ImageLengthMismatch,
    ElfValidationFailed,
    EntryAddressMismatch,
    SegmentTableMalformed,
    TooManyLoadSegments,
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

fn read_u16_le(image: &[u8], offset: usize) -> Result<u16, InitramfsManifestError> {
    let end = offset
        .checked_add(2)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let bytes = image
        .get(offset..end)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(bytes);
    Ok(u16::from_le_bytes(raw))
}

fn read_u32_le(image: &[u8], offset: usize) -> Result<u32, InitramfsManifestError> {
    let end = offset
        .checked_add(4)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let bytes = image
        .get(offset..end)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

fn read_u64_le(image: &[u8], offset: usize) -> Result<u64, InitramfsManifestError> {
    let end = offset
        .checked_add(8)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let bytes = image
        .get(offset..end)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(raw))
}

fn parse_load_segments(image: &[u8]) -> Result<ServiceElfLaunchPlan, InitramfsManifestError> {
    const PT_LOAD: u32 = 1;
    const ELF64_PHDR_SIZE: usize = 56;
    if image.len() < 64 {
        return Err(InitramfsManifestError::SegmentTableMalformed);
    }
    let phoff = read_u64_le(image, 32)? as usize;
    let phentsize = read_u16_le(image, 54)? as usize;
    let phnum = read_u16_le(image, 56)? as usize;
    if phnum == 0 || phentsize < ELF64_PHDR_SIZE {
        return Err(InitramfsManifestError::SegmentTableMalformed);
    }
    let ph_table_size = phnum
        .checked_mul(phentsize)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    let ph_end = phoff
        .checked_add(ph_table_size)
        .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
    if ph_end > image.len() {
        return Err(InitramfsManifestError::SegmentTableMalformed);
    }

    let mut load_segments = [None; MAX_LOAD_SEGMENTS];
    let mut load_segment_count = 0usize;
    for idx in 0..phnum {
        let base = phoff
            .checked_add(
                idx.checked_mul(phentsize)
                    .ok_or(InitramfsManifestError::SegmentTableMalformed)?,
            )
            .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
        if read_u32_le(image, base)? != PT_LOAD {
            continue;
        }
        if load_segment_count >= MAX_LOAD_SEGMENTS {
            return Err(InitramfsManifestError::TooManyLoadSegments);
        }
        let segment = ElfLoadSegmentPlan {
            flags: read_u32_le(image, base + 4)?,
            file_offset: read_u64_le(image, base + 8)?,
            virt_addr: read_u64_le(image, base + 16)?,
            file_size: read_u64_le(image, base + 32)?,
            mem_size: read_u64_le(image, base + 40)?,
        };
        if segment.file_size > segment.mem_size {
            return Err(InitramfsManifestError::SegmentTableMalformed);
        }
        let seg_end = segment
            .file_offset
            .checked_add(segment.file_size)
            .ok_or(InitramfsManifestError::SegmentTableMalformed)?;
        if seg_end as usize > image.len() {
            return Err(InitramfsManifestError::SegmentTableMalformed);
        }
        load_segments[load_segment_count] = Some(segment);
        load_segment_count += 1;
    }
    if load_segment_count == 0 {
        return Err(InitramfsManifestError::SegmentTableMalformed);
    }
    Ok(ServiceElfLaunchPlan {
        manifest: ManifestEntryWire {
            path_ptr: 0,
            file_len: 0,
            entry_addr: 0,
            abi: 0,
            flags: 0,
        },
        validated_entry: 0,
        load_segment_count,
        load_segments,
    })
}

fn resolve_manifest_image<'a>(
    images: &'a [(u64, &'a [u8])],
    path_ptr: u64,
) -> Result<&'a [u8], InitramfsManifestError> {
    images
        .iter()
        .find(|(path, _)| *path == path_ptr)
        .map(|(_, image)| *image)
        .ok_or(InitramfsManifestError::MissingImagePayload)
}

fn build_service_launch_plan(
    manifest: ManifestEntryWire,
    image: &[u8],
) -> Result<ServiceElfLaunchPlan, InitramfsManifestError> {
    if image.len() as u64 != manifest.file_len {
        return Err(InitramfsManifestError::ImageLengthMismatch);
    }
    let validated = ElfImageInfo::parse(manifest.path_ptr, image)
        .map_err(|_| InitramfsManifestError::ElfValidationFailed)?;
    if validated.entry != manifest.entry_addr {
        return Err(InitramfsManifestError::EntryAddressMismatch);
    }
    let mut launch_plan = parse_load_segments(image)?;
    launch_plan.manifest = manifest;
    launch_plan.validated_entry = validated.entry;
    Ok(launch_plan)
}

pub fn build_core_service_elf_launch_plan(
    manifest_bytes: &[u8],
    images: &[(u64, &[u8])],
) -> Result<CoreServiceElfLaunchPlan, InitramfsManifestError> {
    let manifest = parse_core_service_manifest(manifest_bytes)?;
    Ok(CoreServiceElfLaunchPlan {
        init: build_service_launch_plan(
            manifest.init,
            resolve_manifest_image(images, manifest.init.path_ptr)?,
        )?,
        process_manager: build_service_launch_plan(
            manifest.process_manager,
            resolve_manifest_image(images, manifest.process_manager.path_ptr)?,
        )?,
        vfs: build_service_launch_plan(
            manifest.vfs,
            resolve_manifest_image(images, manifest.vfs.path_ptr)?,
        )?,
        supervisor: build_service_launch_plan(
            manifest.supervisor,
            resolve_manifest_image(images, manifest.supervisor.path_ptr)?,
        )?,
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

    fn synthetic_elf_image(entry: u64) -> [u8; 192] {
        let mut image = [0u8; 192];
        image[..4].copy_from_slice(b"\x7FELF");
        image[4] = 2; // ELFCLASS64
        image[5] = 1; // little-endian
        image[6] = 1; // version
        image[7] = 0; // SYSV ABI
        image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        image[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // EM_X86_64
        image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
        image[24..32].copy_from_slice(&entry.to_le_bytes());
        image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        image[52..54].copy_from_slice(&(64u16).to_le_bytes()); // e_ehsize
        image[54..56].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
        image[56..58].copy_from_slice(&(2u16).to_le_bytes()); // e_phnum

        // PT_LOAD #0
        let ph0 = 64usize;
        image[ph0..ph0 + 4].copy_from_slice(&1u32.to_le_bytes());
        image[ph0 + 4..ph0 + 8].copy_from_slice(&5u32.to_le_bytes());
        image[ph0 + 8..ph0 + 16].copy_from_slice(&176u64.to_le_bytes());
        image[ph0 + 16..ph0 + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes());
        image[ph0 + 32..ph0 + 40].copy_from_slice(&8u64.to_le_bytes());
        image[ph0 + 40..ph0 + 48].copy_from_slice(&16u64.to_le_bytes());
        image[ph0 + 48..ph0 + 56].copy_from_slice(&0x1000u64.to_le_bytes());

        // PT_LOAD #1
        let ph1 = 120usize;
        image[ph1..ph1 + 4].copy_from_slice(&1u32.to_le_bytes());
        image[ph1 + 4..ph1 + 8].copy_from_slice(&6u32.to_le_bytes());
        image[ph1 + 8..ph1 + 16].copy_from_slice(&184u64.to_le_bytes());
        image[ph1 + 16..ph1 + 24]
            .copy_from_slice(&((entry & !0xFFF).saturating_add(0x1000)).to_le_bytes());
        image[ph1 + 32..ph1 + 40].copy_from_slice(&8u64.to_le_bytes());
        image[ph1 + 40..ph1 + 48].copy_from_slice(&32u64.to_le_bytes());
        image[ph1 + 48..ph1 + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        image[176..192].copy_from_slice(&[0x90; 16]);
        image
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

    #[test]
    fn core_service_elf_launch_plan_validates_manifest_and_extracts_load_segments() {
        let entries = baseline_entries();
        let init = synthetic_elf_image(entries[0].entry_addr);
        let proc = synthetic_elf_image(entries[1].entry_addr);
        let vfs = synthetic_elf_image(entries[2].entry_addr);
        let supervisor = synthetic_elf_image(entries[3].entry_addr);
        let manifest_bytes = encode_manifest(&[
            ManifestEntryWire {
                file_len: init.len() as u64,
                ..entries[0]
            },
            ManifestEntryWire {
                file_len: proc.len() as u64,
                ..entries[1]
            },
            ManifestEntryWire {
                file_len: vfs.len() as u64,
                ..entries[2]
            },
            ManifestEntryWire {
                file_len: supervisor.len() as u64,
                ..entries[3]
            },
        ]);
        let images = [
            (INITRAMFS_INIT_PATH_PTR, init.as_slice()),
            (INITRAMFS_PROC_MGR_PATH_PTR, proc.as_slice()),
            (INITRAMFS_VFS_PATH_PTR, vfs.as_slice()),
            (INITRAMFS_SUPERVISOR_PATH_PTR, supervisor.as_slice()),
        ];

        let plan = build_core_service_elf_launch_plan(&manifest_bytes, &images).expect("plan");
        assert_eq!(plan.init.validated_entry, entries[0].entry_addr);
        assert_eq!(plan.process_manager.validated_entry, entries[1].entry_addr);
        assert_eq!(plan.vfs.validated_entry, entries[2].entry_addr);
        assert_eq!(plan.supervisor.validated_entry, entries[3].entry_addr);
        assert_eq!(plan.init.load_segment_count, 2);
        assert_eq!(plan.process_manager.load_segment_count, 2);
        assert_eq!(plan.vfs.load_segment_count, 2);
        assert_eq!(plan.supervisor.load_segment_count, 2);
    }

    #[test]
    fn core_service_elf_launch_plan_rejects_entry_mismatch_or_missing_image() {
        let entries = baseline_entries();
        let init = synthetic_elf_image(0x401111);
        let manifest_bytes = encode_manifest(&[
            ManifestEntryWire {
                file_len: init.len() as u64,
                ..entries[0]
            },
            entries[1],
            entries[2],
            entries[3],
        ]);
        let images = [(INITRAMFS_INIT_PATH_PTR, init.as_slice())];
        assert_eq!(
            build_core_service_elf_launch_plan(&manifest_bytes, &images),
            Err(InitramfsManifestError::EntryAddressMismatch)
        );
    }
}
