// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const SBI_EXT_BASE: usize = 0x10;
pub const SBI_EXT_HSM: usize = 0x48534D;
/// QEMU-SMP3 — SBI 1.0 IPI extension ("sPI"). An IPI manifests at each target hart as a
/// SUPERVISOR SOFTWARE interrupt (`sip.SSIP`); OpenSBI raises it on the target from M-mode, and
/// the target's supervisor code clears it.
pub const SBI_EXT_IPI: usize = 0x735049;
/// QEMU-SMP3 — SBI 1.0 RFENCE extension ("RFNC").
pub const SBI_EXT_RFENCE: usize = 0x52464E43;

const SBI_BASE_PROBE_EXTENSION_FID: usize = 3;
const SBI_HSM_HART_START_FID: usize = 0;
const SBI_IPI_SEND_IPI_FID: usize = 0;
const SBI_RFENCE_REMOTE_SFENCE_VMA_ASID_FID: usize = 2;
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

/// QEMU-SMP3 — `sbi_send_ipi(hart_mask, hart_mask_base)`: raise a supervisor software interrupt
/// on every hart in the mask. The spec's success means the IPI was sent; it says nothing about
/// when the target takes it, which the receiver's own record establishes.
pub fn send_ipi(hart_mask: usize, hart_mask_base: usize) -> Result<(), SbiError> {
    let ret = sbi_call(
        SBI_EXT_IPI,
        SBI_IPI_SEND_IPI_FID,
        [hart_mask, hart_mask_base, 0, 0, 0, 0],
    );
    match SbiError::from_error_code(ret.error) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// QEMU-SMP3 — `sbi_remote_sfence_vma_asid(hart_mask, hart_mask_base, start, size, asid)`: have
/// every hart in the mask execute `SFENCE.VMA` for `[start, start + size)` in `asid`.
///
/// **The completion contract, derived rather than assumed.** SBI 1.0 defines `SBI_SUCCESS` as
/// "IPI successfully sent to targeted harts" and does not state when the remote fences have
/// executed. OpenSBI v1.3 — the firmware this port runs on — returns only after they have:
/// `sbi_tlb_request` sends through `sbi_ipi_send_many`, which calls the TLB event's `sync`
/// (`tlb_sync`), and that spins until every target has processed the request. So `Ok` here means
/// "completed on every target" ON THIS FIRMWARE, and every error — including one this port does
/// not name — is a failure the caller must treat as NOT completed.
pub fn remote_sfence_vma_asid(
    hart_mask: usize,
    hart_mask_base: usize,
    start: usize,
    size: usize,
    asid: usize,
) -> Result<(), SbiError> {
    let ret = sbi_call(
        SBI_EXT_RFENCE,
        SBI_RFENCE_REMOTE_SFENCE_VMA_ASID_FID,
        [hart_mask, hart_mask_base, start, size, asid, 0],
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
    fn the_ipi_and_rfence_identities_are_the_sbi_1_0_ones() {
        assert_eq!(SBI_EXT_IPI, 0x735049);
        assert_eq!(SBI_EXT_RFENCE, 0x52464E43);
        assert_eq!(SBI_RFENCE_REMOTE_SFENCE_VMA_ASID_FID, 2);
        assert_eq!(SBI_IPI_SEND_IPI_FID, 0);
        // Off target the calls report NotSupported — never a fabricated success.
        assert_eq!(send_ipi(1, 0), Err(SbiError::NotSupported));
        assert_eq!(
            remote_sfence_vma_asid(1, 0, 0x1000, 0x1000, 5),
            Err(SbiError::NotSupported)
        );
    }

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
