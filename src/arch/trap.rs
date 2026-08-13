// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::vm::VirtAddr;

pub type IrqNumber = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultAccess {
    Read,
    Write,
    /// Instruction fetch from a non-executable page.
    /// RISC-V: instruction page fault / fetch-side fault classification from
    /// `scause`.
    /// x86-64: `#PF` with the instruction-fetch bit set in the error code.
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub addr: VirtAddr,
    pub access: FaultAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    Syscall,
    PageFault,
    TimerInterrupt,
    ExternalInterrupt,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapAction {
    DispatchSyscall,
    HandlePageFault,
    TickScheduler,
    RouteIrq,
    Unhandled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapEvent {
    Syscall,
    PageFault(FaultInfo),
    TimerInterrupt,
    ExternalInterrupt(IrqNumber),
    Unknown { arch_code: u64 },
}

impl TrapEvent {
    pub const fn trap(&self) -> Trap {
        match self {
            Self::Syscall => Trap::Syscall,
            Self::PageFault(_) => Trap::PageFault,
            Self::TimerInterrupt => Trap::TimerInterrupt,
            Self::ExternalInterrupt(_) => Trap::ExternalInterrupt,
            Self::Unknown { .. } => Trap::Unknown,
        }
    }

    pub const fn fault(&self) -> Option<FaultInfo> {
        match self {
            Self::PageFault(fault) => Some(*fault),
            _ => None,
        }
    }

    pub const fn irq(&self) -> Option<IrqNumber> {
        match self {
            Self::ExternalInterrupt(irq) => Some(*irq),
            _ => None,
        }
    }

    pub const fn unknown_code(&self) -> Option<u64> {
        match self {
            Self::Unknown { arch_code } => Some(*arch_code),
            _ => None,
        }
    }
}

/// Routing is currently 1:1 with trap kind. Kept as a separate action enum for
/// future non-trivial mappings.
pub fn route_trap(event: &TrapEvent) -> TrapAction {
    match event {
        TrapEvent::Syscall => TrapAction::DispatchSyscall,
        TrapEvent::PageFault(_) => TrapAction::HandlePageFault,
        TrapEvent::TimerInterrupt => TrapAction::TickScheduler,
        TrapEvent::ExternalInterrupt(_) => TrapAction::RouteIrq,
        TrapEvent::Unknown { .. } => TrapAction::Unhandled,
    }
}

/// U6 §3 — the AUTHORITATIVE AArch64 synchronous-exception resume-PC rule.
///
/// **The rule: a syscall resume PC is the raw vector `ELR_EL1`, unconditionally. No syscall
/// number, no result lane, and no blocking outcome ever adds an offset to it.**
///
/// This is not a preference; it is what the surrounding machine already does, established from
/// the vector and bridge code rather than from prose:
///
/// * The exception vector reads `mrs x9, elr_el1` and stores it verbatim into the vector frame
///   (`Aarch64VectorFrame::elr_el1`, offset 256) and into `LAST_VECTOR_RAW_ELR` via
///   `yarm_aarch64_vector_elr_marker`. It applies **no** adjustment
///   (`arch/aarch64/boot.rs`, vector prologue).
/// * The bridge seeds `TrapFrame::saved_pc` from that same raw value
///   (`trap_frame.set_saved_pc(frame.elr_el1 as usize)`), and on the way out writes the
///   trapframe's `saved_pc` straight back (`write_trapframe_back_to_vector_frame`) for the
///   epilogue's `msr elr_el1, x9`. Again no adjustment anywhere.
/// * Therefore the *only* thing that decides where userspace resumes is the hardware `ELR_EL1`,
///   and on ARMv8-A the synchronous-exception `ELR_EL1` for an `SVC` is the **preferred return
///   address** — the instruction FOLLOWING the `SVC`, not the `SVC` itself.
///
/// Stage 195B proved this empirically on real hardware/QEMU rather than by reading the ARM ARM:
/// the split-finalize path used `ELR + 4`, which over-advanced by one instruction and skipped
/// the caller's return-register load (`mov rN, x0`). `DebugLog` tolerated it because it ignores
/// its return value; the moment a return-value-checking class (`InitramfsReadChunk`, NR 27) went
/// live it returned a stale register. Removing the `+4` fixed it (PM's NR 27 self-probe went
/// from `Internal` to `bytes=16`), and `split_finalize_handled_syscall` has used the raw `ELR`
/// ever since.
///
/// **The contradiction this repair closes.** The global handler kept a stale `needs_plus4` /
/// `ipc_recv_plus4` special case whose comment asserted the exact opposite premise — that
/// `ELR_EL1` "holds the `SVC` instruction address itself" and that a blocked `IpcRecv` therefore
/// "retries the `SVC` on wakeup". Both halves were wrong under the rule above: the extra `+4` on
/// a successful `IpcRecv` over-advanced past the caller's return-register load exactly as Stage
/// 195B described, and no blocked syscall has ever re-executed its `SVC` — a blocked task's
/// `saved_pc` is already past it, which is precisely why blocked classes must publish a
/// completion (`BlockedSyscallCompletion`) to supply the resumed result instead of re-running the
/// instruction. U6 depends on that being true for `IpcSend`, so the contradiction is resolved
/// here in favour of the code that the machine actually executes.
///
/// The returned `&'static str` is the telemetry reason lane only; it never influences the PC.
pub fn aarch64_syscall_elr_policy(
    raw_vector_return_pc: usize,
    syscall_nr: usize,
) -> (usize, &'static str) {
    let reason = if syscall_nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR {
        "debug_log_raw"
    } else {
        "raw"
    };
    (raw_vector_return_pc, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trap_router_maps_syscall() {
        assert_eq!(route_trap(&TrapEvent::Syscall), TrapAction::DispatchSyscall);
    }

    #[test]
    fn trap_event_can_carry_irq_number() {
        let event = TrapEvent::ExternalInterrupt(7);
        assert_eq!(event.irq(), Some(7));
        assert_eq!(event.fault(), None);
    }

    // ── U6 §3 — AArch64 synchronous-exception resume-PC rule ─────────────────────────────────
    //
    // The repaired rule is that a syscall resume PC is the raw vector `ELR_EL1`, which already
    // names the instruction FOLLOWING the `SVC`. "Advance exactly once" therefore means the
    // resume PC equals the raw ELR: one instruction past the `SVC`, never two.

    const SVC_ADDR: usize = 0x0040_307c;
    /// What the hardware places in `ELR_EL1` for a synchronous `SVC` exception: the preferred
    /// return address, i.e. the instruction after the `SVC`. One advance, applied by the CPU.
    const RAW_ELR: usize = SVC_ADDR + 4;

    #[test]
    fn u6_stage3_immediate_success_advances_exactly_once() {
        // An `IpcRecv` that completes immediately used to take the `ipc_recv_plus4` lane
        // (`nr == IpcRecv && !frame.is_error()`), landing at SVC+8 and skipping the caller's
        // return-register load. It must now land at SVC+4.
        let nr = crate::kernel::syscall::Syscall::IpcRecv as usize;
        let (pc, reason) = aarch64_syscall_elr_policy(RAW_ELR, nr);
        assert_eq!(
            pc, RAW_ELR,
            "a successful IpcRecv must resume one instruction past the SVC, not two"
        );
        assert_eq!(pc, SVC_ADDR + 4, "exactly one instruction advance");
        assert_ne!(
            pc,
            SVC_ADDR + 8,
            "the retired ipc_recv_plus4 over-advance must not be reachable"
        );
        assert_eq!(reason, "raw");
    }

    #[test]
    fn u6_stage3_immediate_refusal_advances_exactly_once() {
        // A definitive error return (the refusal lane U6's immediate-error disposition uses)
        // advances identically: the ELR rule does not consult the result lane at all.
        let nr = crate::kernel::syscall::Syscall::IpcSend as usize;
        let (pc, _) = aarch64_syscall_elr_policy(RAW_ELR, nr);
        assert_eq!(
            pc,
            SVC_ADDR + 4,
            "an immediate refusal must advance exactly once, exactly like a success"
        );
    }

    #[test]
    fn u6_stage3_blocked_resume_advances_exactly_once() {
        // A blocked syscall never re-executes its `SVC`: the frame it is resumed from carries
        // the same raw ELR the trap arrived with. Whatever the class, the policy contributes no
        // additional offset, so a resumed task lands one instruction past the `SVC` and consumes
        // its published completion rather than re-running the instruction.
        for nr in [
            crate::kernel::syscall::Syscall::IpcRecv as usize,
            crate::kernel::syscall::Syscall::IpcSend as usize,
        ] {
            let (pc, _) = aarch64_syscall_elr_policy(RAW_ELR, nr);
            assert_eq!(
                pc,
                SVC_ADDR + 4,
                "blocked-class resume must advance exactly once for nr={nr}"
            );
        }
    }

    #[test]
    fn u6_stage3_elr_policy_is_unconditional_across_every_syscall_number() {
        // The rule is unconditional: no syscall number gets an offset. Only the telemetry reason
        // lane distinguishes DebugLog, and it cannot influence the PC.
        for nr in 0..64usize {
            let (pc, reason) = aarch64_syscall_elr_policy(RAW_ELR, nr);
            assert_eq!(pc, RAW_ELR, "nr={nr} must not adjust the resume PC");
            let expected = if nr == crate::kernel::syscall::SYSCALL_DEBUG_LOG_NR {
                "debug_log_raw"
            } else {
                "raw"
            };
            assert_eq!(reason, expected, "telemetry reason lane for nr={nr}");
        }
    }

    #[test]
    fn u6_stage3_no_plus4_special_case_survives_in_the_global_handler() {
        // The stale premise ("ELR_EL1 for SVC holds the SVC instruction address itself" / "the
        // task then retries the SVC on wakeup") and its `ipc_recv_plus4` lane are gone, and no
        // syscall-resume path re-adds an offset.
        let src = include_str!("aarch64/trap.rs");
        assert!(
            !src.contains("ipc_recv_plus4"),
            "the ipc_recv_plus4 telemetry lane must be retired"
        );
        assert!(
            !src.contains("needs_plus4"),
            "the needs_plus4 special case must be retired"
        );
        assert!(
            !src.contains("retries the SVC on wakeup"),
            "the stale SVC-retry premise must be removed"
        );
        assert!(
            !src.contains("raw_vector_return_pc.wrapping_add(4)"),
            "no syscall resume path may add +4 to the raw vector ELR"
        );
        assert!(
            src.contains("last_vector_raw_elr() as usize"),
            "the resume PC must still come from the raw vector ELR"
        );
    }
}
