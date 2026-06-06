// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

extern crate alloc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpioError {
    Truncated,
    InvalidMagic,
    InvalidHex,
    InvalidNameLen,
    InvalidAlignment,
}

#[derive(Debug, Clone, Copy)]
pub struct CpioArchive<'a> {
    bytes: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub struct CpioEntry<'a> {
    pub name: &'a [u8],
    pub mode: u32,
    data: &'a [u8],
}

impl<'a> CpioEntry<'a> {
    pub const fn file_data(&self) -> &'a [u8] {
        self.data
    }

    pub const fn is_regular_file(&self) -> bool {
        (self.mode & 0o170000) == 0o100000
    }
}

impl<'a> CpioArchive<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn entries(&self) -> CpioEntries<'a> {
        CpioEntries {
            bytes: self.bytes,
            off: 0,
            done: false,
            errored: false,
        }
    }

    pub fn find(&self, path: &str) -> Result<Option<CpioEntry<'a>>, CpioError> {
        let needle = path.strip_prefix('/').unwrap_or(path).as_bytes();
        let mut iter = self.entries();
        while let Some(entry) = iter.next() {
            let entry = entry?;
            if entry.name == needle {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }
}

pub struct CpioEntries<'a> {
    bytes: &'a [u8],
    off: usize,
    done: bool,
    errored: bool,
}

fn parse_hex_u32(field: &[u8]) -> Result<u32, CpioError> {
    let mut out = 0u32;
    for &b in field {
        out <<= 4;
        out |= match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return Err(CpioError::InvalidHex),
        };
    }
    Ok(out)
}

fn align4(v: usize) -> Result<usize, CpioError> {
    v.checked_add(3)
        .map(|x| x & !3)
        .ok_or(CpioError::InvalidAlignment)
}

impl<'a> Iterator for CpioEntries<'a> {
    type Item = Result<CpioEntry<'a>, CpioError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.errored {
            return None;
        }
        let Some(header_end) = self.off.checked_add(110) else {
            self.errored = true;
            return Some(Err(CpioError::Truncated));
        };
        let header = match self.bytes.get(self.off..header_end) {
            Some(header) => header,
            None => {
                self.errored = true;
                return Some(Err(CpioError::Truncated));
            }
        };
        if &header[0..6] != b"070701" {
            self.errored = true;
            return Some(Err(CpioError::InvalidMagic));
        }
        let mode = match parse_hex_u32(&header[14..22]) {
            Ok(v) => v,
            Err(e) => {
                self.errored = true;
                return Some(Err(e));
            }
        };
        let file_size = match parse_hex_u32(&header[54..62]) {
            Ok(v) => v as usize,
            Err(e) => {
                self.errored = true;
                return Some(Err(e));
            }
        };
        let name_len = match parse_hex_u32(&header[94..102]) {
            Ok(v) if v > 0 => v as usize,
            Ok(_) => {
                self.errored = true;
                return Some(Err(CpioError::InvalidNameLen));
            }
            Err(e) => {
                self.errored = true;
                return Some(Err(e));
            }
        };
        let name_off = self.off + 110;
        let name_raw = match self.bytes.get(name_off..name_off + name_len) {
            Some(v) => v,
            None => {
                self.errored = true;
                return Some(Err(CpioError::Truncated));
            }
        };
        let name = &name_raw[..name_len - 1];
        let data_off = match align4(name_off + name_len) {
            Ok(v) => v,
            Err(e) => {
                self.errored = true;
                return Some(Err(e));
            }
        };
        let data = match self.bytes.get(data_off..data_off + file_size) {
            Some(v) => v,
            None => {
                self.errored = true;
                return Some(Err(CpioError::Truncated));
            }
        };
        self.off = match align4(data_off + file_size) {
            Ok(v) => v,
            Err(e) => {
                self.errored = true;
                return Some(Err(e));
            }
        };
        if name == b"TRAILER!!!" {
            self.done = true;
            return None;
        }
        Some(Ok(CpioEntry { name, mode, data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec::Vec;

    fn push_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let namesz = name.len() + 1;
        let mut h = [0u8; 110];
        h[0..6].copy_from_slice(b"070701");
        h[14..22].copy_from_slice(format!("{mode:08x}").as_bytes());
        h[54..62].copy_from_slice(format!("{:08x}", data.len()).as_bytes());
        h[94..102].copy_from_slice(format!("{namesz:08x}").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn sample() -> Vec<u8> {
        let mut out = Vec::new();
        push_entry(&mut out, "init", 0o100755, b"\x7fELF....");
        push_entry(&mut out, "etc", 0o040755, b"");
        push_entry(&mut out, "TRAILER!!!", 0, b"");
        out
    }

    #[test]
    fn finds_init() {
        let bytes = sample();
        let a = CpioArchive::new(&bytes);
        let init = a.find("/init").expect("parse").expect("init");
        assert!(init.is_regular_file());
        assert_eq!(init.file_data(), b"\x7fELF....");
    }

    #[test]
    fn missing_file() {
        let bytes = sample();
        let a = CpioArchive::new(&bytes);
        assert!(a.find("/nope").expect("parse").is_none());
    }

    #[test]
    fn malformed_header() {
        let mut s = sample();
        s[0] = b'0';
        s[1] = b'0';
        s[2] = b'0';
        let mut it = CpioArchive::new(&s).entries();
        assert!(matches!(
            it.next().expect("entry"),
            Err(CpioError::InvalidMagic)
        ));
    }

    #[test]
    fn alignment_is_handled() {
        let mut out = Vec::new();
        push_entry(&mut out, "ab", 0o100644, b"123");
        push_entry(&mut out, "TRAILER!!!", 0, b"");
        let mut it = CpioArchive::new(&out).entries();
        let e = it.next().expect("one").expect("ok");
        assert_eq!(e.name, b"ab");
        assert_eq!(e.file_data(), b"123");
    }

    #[test]
    fn short_archive_is_truncated() {
        let mut entries = CpioArchive::new(b"not-cpio").entries();
        assert!(matches!(entries.next(), Some(Err(CpioError::Truncated))));
    }

    #[test]
    fn missing_trailer_is_truncated() {
        let mut out = Vec::new();
        push_entry(&mut out, "init", 0o100755, b"\x7fELF....");
        let mut entries = CpioArchive::new(&out).entries();
        assert!(entries.next().expect("init").is_ok());
        assert!(matches!(entries.next(), Some(Err(CpioError::Truncated))));
    }
}
