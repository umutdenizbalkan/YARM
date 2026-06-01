// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const SBI_EXT_BASE: usize = 0x10;
pub const SBI_EXT_HSM: usize = 0x48534D;

const SBI_BASE_PROBE_EXTENSION_FID: usize = 3;
const SBI_HSM_HART_START_FID: usize = 0;
#[allow(dead_code)]
const SBI_HSM_HART_GET_STATUS_FID: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbiRet {
    pub error: isize,
    pub value: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SbiError {
    Failed,
    NotSupported,
    InvalidParam,
    Denied,
    InvalidAddress,
    AlreadyAvailable,
    AlreadyStarted,
    AlreadyStopped,
    NoShmem,
    Unknown(isize),
}

impl SbiError {
    pub const fn from_error_code(code: isize) -> Option<Self> {
        match code {
            0 => None,
            -1 => Some(Self::Failed),
            -2 => Some(Self::NotSupported),
            -3 => Some(Self::InvalidParam),
            -4 => Some(Self::Denied),
            -5 => Some(Self::InvalidAddress),
            -6 => Some(Self::AlreadyAvailable),
            -7 => Some(Self::AlreadyStarted),
            -8 => Some(Self::AlreadyStopped),
            -9 => Some(Self::NoShmem),
            other => Some(Self::Unknown(other)),
        }
    }
}

#[cfg(all(not(feature = "hosted-dev"), target_arch = "riscv64"))]
#[inline]
fn sbi_call(extension: usize, function: usize, args: [usize; 6]) -> SbiRet {
    let mut a0 = args[0];
    let mut a1 = args[1];
    unsafe {
        core::arch::asm!(
            "ecall",
            inout("a0") a0,
            inout("a1") a1,
            in("a2") args[2],
            in("a3") args[3],
            in("a4") args[4],
            in("a5") args[5],
            in("a6") function,
            in("a7") extension,
            options(nostack)
        );
    }
    SbiRet {
        error: a0 as isize,
        value: a1,
    }
}

#[cfg(any(feature = "hosted-dev", not(target_arch = "riscv64")))]
#[inline]
fn sbi_call(_extension: usize, _function: usize, _args: [usize; 6]) -> SbiRet {
    SbiRet {
        error: -2,
        value: 0,
    }
}

pub fn probe_extension(extension: usize) -> Result<usize, SbiError> {
    let ret = sbi_call(
        SBI_EXT_BASE,
        SBI_BASE_PROBE_EXTENSION_FID,
        [extension, 0, 0, 0, 0, 0],
    );
    match SbiError::from_error_code(ret.error) {
        Some(err) => Err(err),
        None => Ok(ret.value),
    }
}

pub fn hsm_hart_start(hart_id: usize, start_addr: usize, opaque: usize) -> Result<(), SbiError> {
    let ret = sbi_call(
        SBI_EXT_HSM,
        SBI_HSM_HART_START_FID,
        [hart_id, start_addr, opaque, 0, 0, 0],
    );
    match SbiError::from_error_code(ret.error) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[allow(dead_code)]
pub fn hsm_hart_get_status(hart_id: usize) -> Result<usize, SbiError> {
    let ret = sbi_call(
        SBI_EXT_HSM,
        SBI_HSM_HART_GET_STATUS_FID,
        [hart_id, 0, 0, 0, 0, 0],
    );
    match SbiError::from_error_code(ret.error) {
        Some(err) => Err(err),
        None => Ok(ret.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_standard_sbi_errors() {
        assert_eq!(SbiError::from_error_code(0), None);
        assert_eq!(SbiError::from_error_code(-2), Some(SbiError::NotSupported));
        assert_eq!(
            SbiError::from_error_code(-6),
            Some(SbiError::AlreadyAvailable)
        );
        assert_eq!(SbiError::from_error_code(-42), Some(SbiError::Unknown(-42)));
    }
}
