// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-CONTEXT1 §2 — the per-task home of a user task's FP/SIMD and user-writable control
//! state, and the pure rules that govern it.
//!
//! # The policy, one per architecture (eager, no lazy ownership)
//!
//! * **x86_64** — the FXSAVE64 image (512 bytes): x87 FCW/FSW/FTW/FOP/FIP/FDP and ST0..ST7, MXCSR,
//!   XMM0..XMM15. CR4.OSFXSR/OSXMMEXCPT are set and CR4.OSXSAVE is not, so FXSAVE is the complete
//!   format for the enabled state; no AVX/XSAVE state exists to save.
//! * **AArch64** — q0..q31 (128 bits each), FPCR, FPSR and TPIDR_EL0 (544 bytes). No SVE.
//! * **RISC-V** — nothing: userspace is soft-float and every U-mode return runs with
//!   `sstatus.FS/VS = Off` (199E-R1F), so there is no user FP state to own.
//!
//! Every trap from user mode captures the live state into the trap's own scratch area in
//! assembly, before any kernel instruction can touch it, and the trap path COMMITS that area to
//! the entering task's home ([`UserFpuState`], a field of its TCB) before anything can block or
//! switch. Every return to user mode LOADS the resuming task's home into the scratch area, which
//! assembly restores after the last kernel instruction. The home is therefore the only saved copy
//! whenever the task is not running, which is what makes blocking, idling, switching and resuming
//! on another path all the same case.
//!
//! The user condition flags travel differently: they are one word, so they ride in the saved
//! register continuation itself (`UserRegisterContext::user_status`), captured and applied by the
//! same owners that carry the GPRs. [`sanitize_user_rflags`] / [`sanitize_user_spsr`] define what
//! a return may hand back.
//!
//! # Ownership rules
//!
//! * A **new TCB** (any spawn, and therefore any reuse of a cleared slot) starts with
//!   [`UserFpuState::initial`]: the architectural reset state, never another task's bytes.
//! * A **new thread** additionally gets `TPIDR_EL0 = tls_base` on AArch64.
//! * **Fork**: the child's home is a copy of the parent's, which the fork syscall's own entry
//!   committed.
//! * **Exec** resets the home to the initial state.
//! * A **failed spawn** restores the reservation's pre-claim home with the rest of its baseline.

/// Bytes in one architecture's image.
#[cfg(target_arch = "x86_64")]
pub const USER_FPU_BYTES: usize = 512;
#[cfg(target_arch = "aarch64")]
pub const USER_FPU_BYTES: usize = 544;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const USER_FPU_BYTES: usize = 0;

/// AArch64 image layout (the vector frame uses the same offsets).
pub const A64_FPCR_OFFSET: usize = 512;
pub const A64_FPSR_OFFSET: usize = 520;
pub const A64_TPIDR_OFFSET: usize = 528;

/// x86_64 FXSAVE field offsets.
pub const FX_FCW_OFFSET: usize = 0;
pub const FX_MXCSR_OFFSET: usize = 24;

/// The x87 control word `fninit` establishes: all exceptions masked, 64-bit precision, nearest.
pub const X86_FCW_INIT: u16 = 0x037F;
/// The MXCSR power-on value: all exceptions masked, round to nearest, no FZ/DAZ.
pub const X86_MXCSR_INIT: u32 = 0x1F80;

/// One task's saved user FP/SIMD and control state.
#[repr(C, align(16))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UserFpuState {
    image: [u8; USER_FPU_BYTES],
}

impl core::fmt::Debug for UserFpuState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UserFpuState")
            .field("bytes", &USER_FPU_BYTES)
            .finish()
    }
}

impl Default for UserFpuState {
    fn default() -> Self {
        Self::initial()
    }
}

impl UserFpuState {
    /// The architectural initial state a fresh task is owed.
    pub const fn initial() -> Self {
        #[allow(unused_mut)]
        let mut image = [0u8; USER_FPU_BYTES];
        #[cfg(target_arch = "x86_64")]
        {
            let fcw = X86_FCW_INIT.to_le_bytes();
            image[FX_FCW_OFFSET] = fcw[0];
            image[FX_FCW_OFFSET + 1] = fcw[1];
            let mxcsr = X86_MXCSR_INIT.to_le_bytes();
            image[FX_MXCSR_OFFSET] = mxcsr[0];
            image[FX_MXCSR_OFFSET + 1] = mxcsr[1];
            image[FX_MXCSR_OFFSET + 2] = mxcsr[2];
            image[FX_MXCSR_OFFSET + 3] = mxcsr[3];
        }
        Self { image }
    }

    /// The initial state of a new thread whose TLS base is `tls` (AArch64: `TPIDR_EL0 = tls`).
    pub fn initial_for_thread(tls: u64) -> Self {
        #[allow(unused_mut)]
        let mut s = Self::initial();
        #[cfg(target_arch = "aarch64")]
        s.image[A64_TPIDR_OFFSET..A64_TPIDR_OFFSET + 8].copy_from_slice(&tls.to_le_bytes());
        #[cfg(not(target_arch = "aarch64"))]
        let _ = tls;
        s
    }

    pub const fn bytes(&self) -> &[u8; USER_FPU_BYTES] {
        &self.image
    }

    pub fn bytes_mut(&mut self) -> &mut [u8; USER_FPU_BYTES] {
        &mut self.image
    }

    /// Build a state from an assembly scratch area of the same layout.
    ///
    /// # Safety
    /// `src` must point at `USER_FPU_BYTES` readable bytes.
    pub unsafe fn read_from(src: *const u8) -> Self {
        let mut s = Self::initial();
        // SAFETY: caller contract; the destination is a distinct local.
        unsafe { core::ptr::copy_nonoverlapping(src, s.image.as_mut_ptr(), USER_FPU_BYTES) };
        s
    }

    /// Copy this state into an assembly scratch area of the same layout.
    ///
    /// # Safety
    /// `dst` must point at `USER_FPU_BYTES` writable bytes not aliased by `self`.
    pub unsafe fn write_to(&self, dst: *mut u8) {
        // SAFETY: caller contract.
        unsafe { core::ptr::copy_nonoverlapping(self.image.as_ptr(), dst, USER_FPU_BYTES) };
    }
}

/// x86_64 RFLAGS bits a ring-3 return may hand back from a task's saved continuation: the six
/// arithmetic status flags, DF, AC and ID — every flag ring 3 can set that has no privileged
/// effect. IF is forced on and the reserved bit 1 set; TF, NT, RF, IOPL, VM, VIF and VIP never
/// come back from a saved value.
pub const X86_USER_RFLAGS_MASK: u64 =
    0x1 | 0x4 | 0x10 | 0x40 | 0x80 | 0x800 | 0x400 | 0x4_0000 | 0x20_0000;
pub const X86_RFLAGS_RETURN_FIXED: u64 = 0x202;

/// The RFLAGS a ring-3 return installs for a continuation that saved `saved`. A continuation that
/// never ran (`0`) gets exactly the fresh-entry value `0x202`.
pub const fn sanitize_user_rflags(saved: u64) -> u64 {
    (saved & X86_USER_RFLAGS_MASK) | X86_RFLAGS_RETURN_FIXED
}

/// AArch64 PSTATE bits an EL0 return may hand back: NZCV. Every other SPSR bit (mode, DAIF, SS,
/// IL, PAN, ...) comes from the return path's own policy: EL0t with interrupts unmasked.
pub const A64_NZCV_MASK: u64 = 0xF000_0000;

/// The SPSR an EL0 return installs for a continuation that saved `saved`.
pub const fn sanitize_user_spsr(saved: u64) -> u64 {
    saved & A64_NZCV_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_home_is_the_architectural_reset_state() {
        let s = UserFpuState::initial();
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(s.bytes().len(), 512);
            assert_eq!(&s.bytes()[0..2], &0x037Fu16.to_le_bytes());
            assert_eq!(&s.bytes()[24..28], &0x1F80u32.to_le_bytes());
            // FSW, FTW, FOP, FIP, FDP, ST0..7 and XMM0..15 all zero.
            assert!(s.bytes()[2..24].iter().all(|&b| b == 0));
            assert!(s.bytes()[32..].iter().all(|&b| b == 0));
        }
        assert_eq!(UserFpuState::default(), s);
    }

    #[test]
    fn a_new_threads_home_carries_only_its_own_tls() {
        let t = UserFpuState::initial_for_thread(0x7E57_0000);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(
            &t.bytes()[A64_TPIDR_OFFSET..A64_TPIDR_OFFSET + 8],
            &0x7E57_0000u64.to_le_bytes()
        );
        #[cfg(not(target_arch = "aarch64"))]
        assert_eq!(t, UserFpuState::initial());
    }

    #[test]
    fn scratch_area_round_trip_is_exact() {
        let mut s = UserFpuState::initial();
        for (i, b) in s.bytes_mut().iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(13) ^ 0x5A;
        }
        #[repr(C, align(16))]
        struct Area([u8; USER_FPU_BYTES]);
        let mut area = Area([0; USER_FPU_BYTES]);
        unsafe { s.write_to(area.0.as_mut_ptr()) };
        let back = unsafe { UserFpuState::read_from(area.0.as_ptr()) };
        assert_eq!(back, s);
    }

    #[test]
    fn a_ring3_return_keeps_user_flags_and_nothing_privileged() {
        // A fresh continuation gets exactly the first-entry value.
        assert_eq!(sanitize_user_rflags(0), 0x202);
        // Arithmetic flags, DF, AC and ID survive.
        let user = 0x1 | 0x4 | 0x10 | 0x40 | 0x80 | 0x400 | 0x800 | 0x4_0000 | 0x20_0000;
        assert_eq!(sanitize_user_rflags(user), user | 0x202);
        // TF, IOPL, NT, RF, VM, VIF, VIP are dropped; IF is forced on.
        let hostile = 0x100 | 0x3000 | 0x4000 | 0x1_0000 | 0x2_0000 | 0x8_0000 | 0x10_0000;
        assert_eq!(sanitize_user_rflags(hostile), 0x202);
    }

    #[test]
    fn an_el0_return_keeps_nzcv_and_nothing_else() {
        assert_eq!(sanitize_user_spsr(0), 0);
        assert_eq!(sanitize_user_spsr(0xA000_0000), 0xA000_0000);
        // Mode EL1h, DAIF, SS, IL: never handed back.
        assert_eq!(
            sanitize_user_spsr(0x5000_03C5 | (1 << 21) | (1 << 20)),
            0x5000_0000
        );
    }
}
