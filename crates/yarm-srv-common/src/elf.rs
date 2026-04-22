// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfParseError {
    Malformed,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfImageInfo {
    pub entry: u64,
    pub image_id: u64,
}

impl ElfImageInfo {
    const EI_CLASS: usize = 4;
    const EI_DATA: usize = 5;
    const EI_OSABI: usize = 7;
    const ELFCLASS64: u8 = 2;
    const ELFDATA2LSB: u8 = 1;
    const ELFDATA2MSB: u8 = 2;
    const ELFOSABI_SYSV: u8 = 0;
    const ELFOSABI_GNU: u8 = 3;
    const ELFOSABI_STANDALONE: u8 = 255;
    const ET_EXEC: u16 = 2;
    const ET_DYN: u16 = 3;
    const PT_LOAD: u32 = 1;
    const ELF64_EHDR_SIZE: usize = 64;
    const ELF64_PHDR_SIZE: usize = 56;

    fn read_u16(image: &[u8], offset: usize, big_endian: bool) -> Result<u16, ElfParseError> {
        let end = offset.checked_add(2).ok_or(ElfParseError::Malformed)?;
        let bytes = image.get(offset..end).ok_or(ElfParseError::Malformed)?;
        let mut raw = [0u8; 2];
        raw.copy_from_slice(bytes);
        Ok(if big_endian {
            u16::from_be_bytes(raw)
        } else {
            u16::from_le_bytes(raw)
        })
    }

    fn read_u32(image: &[u8], offset: usize, big_endian: bool) -> Result<u32, ElfParseError> {
        let end = offset.checked_add(4).ok_or(ElfParseError::Malformed)?;
        let bytes = image.get(offset..end).ok_or(ElfParseError::Malformed)?;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(bytes);
        Ok(if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        })
    }

    fn read_u64(image: &[u8], offset: usize, big_endian: bool) -> Result<u64, ElfParseError> {
        let end = offset.checked_add(8).ok_or(ElfParseError::Malformed)?;
        let bytes = image.get(offset..end).ok_or(ElfParseError::Malformed)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        Ok(if big_endian {
            u64::from_be_bytes(raw)
        } else {
            u64::from_le_bytes(raw)
        })
    }

    fn expected_machine() -> u16 {
        #[cfg(target_arch = "x86_64")]
        {
            return 0x3E;
        }
        #[cfg(target_arch = "riscv64")]
        {
            return 0xF3;
        }
        #[cfg(target_arch = "aarch64")]
        {
            return 0xB7;
        }
        #[allow(unreachable_code)]
        0
    }

    pub fn parse(image_id: u64, image: &[u8]) -> Result<Self, ElfParseError> {
        if image.len() < Self::ELF64_EHDR_SIZE {
            return Err(ElfParseError::Malformed);
        }
        if &image[..4] != b"\x7FELF" {
            return Err(ElfParseError::Malformed);
        }
        if image[Self::EI_CLASS] != Self::ELFCLASS64 {
            return Err(ElfParseError::Unsupported);
        }
        let data = image[Self::EI_DATA];
        let big_endian = match data {
            Self::ELFDATA2LSB => false,
            Self::ELFDATA2MSB => true,
            _ => return Err(ElfParseError::Unsupported),
        };
        match image[Self::EI_OSABI] {
            Self::ELFOSABI_SYSV | Self::ELFOSABI_GNU | Self::ELFOSABI_STANDALONE => {}
            _ => return Err(ElfParseError::Unsupported),
        }

        let e_type = Self::read_u16(image, 16, big_endian)?;
        if e_type != Self::ET_EXEC && e_type != Self::ET_DYN {
            return Err(ElfParseError::Unsupported);
        }
        let e_machine = Self::read_u16(image, 18, big_endian)?;
        if e_machine != Self::expected_machine() {
            return Err(ElfParseError::Unsupported);
        }

        let entry = Self::read_u64(image, 24, big_endian)?;
        let phoff = Self::read_u64(image, 32, big_endian)? as usize;
        let phentsize = Self::read_u16(image, 54, big_endian)? as usize;
        let phnum = Self::read_u16(image, 56, big_endian)? as usize;
        if phnum == 0 || phentsize < Self::ELF64_PHDR_SIZE {
            return Err(ElfParseError::Malformed);
        }
        let ph_table_size = phnum
            .checked_mul(phentsize)
            .ok_or(ElfParseError::Malformed)?;
        let ph_end = phoff
            .checked_add(ph_table_size)
            .ok_or(ElfParseError::Malformed)?;
        if ph_end > image.len() {
            return Err(ElfParseError::Malformed);
        }

        let mut load_segments = 0usize;
        for idx in 0..phnum {
            let base = phoff
                .checked_add(idx.checked_mul(phentsize).ok_or(ElfParseError::Malformed)?)
                .ok_or(ElfParseError::Malformed)?;
            let p_type = Self::read_u32(image, base, big_endian)?;
            if p_type != Self::PT_LOAD {
                continue;
            }
            load_segments = load_segments.saturating_add(1);
            let p_offset = Self::read_u64(image, base + 8, big_endian)? as usize;
            let p_filesz = Self::read_u64(image, base + 32, big_endian)? as usize;
            let p_memsz = Self::read_u64(image, base + 40, big_endian)? as usize;
            if p_filesz > p_memsz {
                return Err(ElfParseError::Malformed);
            }
            let seg_end = p_offset
                .checked_add(p_filesz)
                .ok_or(ElfParseError::Malformed)?;
            if seg_end > image.len() {
                return Err(ElfParseError::Malformed);
            }
        }
        if load_segments == 0 {
            return Err(ElfParseError::Malformed);
        }

        Ok(Self { entry, image_id })
    }
}
