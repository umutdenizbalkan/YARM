// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! QEMU-IRQ1 §3/§4 — init's RECEIVER for the RISC-V UART external-interrupt witness, since
//! QEMU-IRQ2 also for the AArch64 PL011 witness, and since QEMU-IRQ3 for the x86_64 16550 witness:
//! ONE receiver, whose only per-port parts are the syscall instruction, the register-checked spin
//! and the isolation probe addresses.
//!
//! Slot-5 selector 30; compiled only with `riscv-uart-irq-witness` on RISC-V,
//! `aarch64-pl011-irq-witness` on AArch64 or `x86_64-uart-irq-witness` on x86_64. The markers keep their IRQ1 names on both ports — they
//! are this receiver's protocol with the host driver, not a claim about the port. The kernel hands it
//! a RECEIVE cap on the notification the UART route targets (slot 13), a private park endpoint
//! (slot 14), and a USER read-only ring page at [`RING_VA`] into which the kernel fixture copies
//! the bytes it drained.
//!
//! One item outstanding at a time, sequence-numbered. For item `seq` the receiver announces
//! `IRQ1_UART_READY seq=… mode=…`, and the host injects exactly one byte, `0x40 + seq`, into the
//! UART's dedicated serial backend only after reading that line. Odd items wait with the hart
//! IDLE (the receiver parks on a deadline, nothing else is runnable); even items wait with the
//! receiver SPINNING in U-mode with six sentinel registers live, so the interrupt lands on user
//! code whose register file must come back intact.
//!
//! What is checked, per item, from the receiver's side of the interface:
//! * a notification is received through NR 5 (the existing object, through the existing receive
//!   syscall), and its label is the line the kernel bound — identity from the claim, not assumed;
//! * exactly one: an immediate second probe is empty;
//! * the DATA sequence separately: the ring's byte count is exactly `seq` and byte `seq-1` is
//!   `0x40 + seq`.
//!
//! After the last item the kernel has turned the source off. The receiver then announces
//! `IRQ1_UART_POSTDISABLE_READY`, the host injects one more byte, and the receiver parks through
//! `POST_ROUNDS` further deadlines — each must time out (the timer still runs), and neither a
//! notification nor a new ring byte may appear (the source really is off). Last, two isolation
//! probes: a disposable child loads from the claim register's window address and must fault, and
//! an anonymous mapping over the window must be refused.

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

pub(super) static RESULT: AtomicU32 = AtomicU32::new(0);

pub(super) const SELECTOR: u32 = 30;
/// Must equal the kernel's `UART_IRQ_WITNESS_RING_VA` (pinned by a hosted test).
pub(super) const RING_VA: usize = 0x2800_0000;
const ITEMS: u32 = 8;
const BYTE_BASE: u8 = 0x40;
/// Park deadline for IDLE items, in scheduler ticks. Long enough that the hart is genuinely idle
/// when the byte arrives, short enough to bound the wait for the next probe.
const IDLE_PARK_TICKS: u64 = 20;
/// Upper bound on waiting for one item — about 60 s of parks, or 60 000 spin/probe rounds.
const MAX_IDLE_ROUNDS: u32 = 300;
const MAX_USER_ROUNDS: u32 = 60_000;
const SPIN_ITERS: u64 = 20_000;
const POST_ROUNDS: u32 = 10;
const POST_PARK_TICKS: u64 = 5;

// Ring word indices (kernel `uart_irq_witness::RING_WORD_*`).
const W_MAGIC: usize = 0;
const W_ARMED: usize = 1;
const W_DISABLED: usize = 2;
const W_BYTES: usize = 3;
const W_CLAIMS: usize = 4;
const W_COMPLETIONS: usize = 5;
const W_IDLE: usize = 6;
const W_USER: usize = 7;
const W_EMPTY: usize = 8;
const W_LINE: usize = 9;
const RING_MAGIC: u32 = 0x4952_5131;
const RING_BYTES_OFFSET: usize = 256;

/// The kernel-only device window's first page (`page_table::DEVICE_WINDOW_BASE`), and the window
/// address of the PLIC S-context claim register (window slot 2, offset 4).
#[cfg(target_arch = "riscv64")]
const DEVICE_WINDOW_BASE: usize = 0x3F_C000_0000;
#[cfg(target_arch = "riscv64")]
const WINDOW_CLAIM_VA: usize = DEVICE_WINDOW_BASE + 2 * 4096 + 4;
/// RISC-V isolation probes: a load from the claim register's window address, and an anonymous
/// mapping over the window.
#[cfg(target_arch = "riscv64")]
const ISOLATION_LOAD_VA: usize = WINDOW_CLAIM_VA;
#[cfg(target_arch = "riscv64")]
const ISOLATION_MAP_VA: usize = DEVICE_WINDOW_BASE;
/// QEMU-IRQ2 — AArch64 maps no new window: the PL011 and GIC pages are the reserved privileged
/// Device leaves every root already carries (identity, VA = PA). The probes are a load from the
/// PL011's read-only flag register (a successful read would change no device state) and an
/// anonymous mapping over the PL011 page.
#[cfg(target_arch = "aarch64")]
const PL011_BASE_VA: usize = 0x0900_0000;
#[cfg(target_arch = "aarch64")]
const ISOLATION_LOAD_VA: usize = PL011_BASE_VA + 0x18;
#[cfg(target_arch = "aarch64")]
const ISOLATION_MAP_VA: usize = PL011_BASE_VA;
/// QEMU-IRQ3 — x86_64: the I/O APIC register window the witness programs, reached by the kernel
/// through its uncached higher-half alias (`platform_layout::IOAPIC_MMIO_BASE`). A user load from
/// it must fault; a user mapping cannot be placed there (it is not a user address). COM1 itself is
/// a PORT, and a ring-3 `in` would raise #GP — which this kernel treats as fatal — so the port side
/// of isolation is shown from live TSS/IOPL state by the kernel fixture instead.
#[cfg(target_arch = "x86_64")]
const ISOLATION_LOAD_VA: usize = 0xFFFF_FFFF_FEC0_0000;
#[cfg(target_arch = "x86_64")]
const ISOLATION_MAP_VA: usize = 0xFFFF_FFFF_FEC0_0000;

const NR_IPC_RECV_TIMEOUT: usize = 5;
const ERR_WOULD_BLOCK: usize = 7;
const ERR_TIMED_OUT: usize = 9;

pub(super) fn armed(slot5: Option<u32>) -> bool {
    matches!(slot5, Some(SELECTOR))
}

fn ring_word(i: usize) -> u32 {
    // SAFETY: the kernel mapped RING_VA USER read-only for this task before it ran.
    unsafe { core::ptr::read_volatile((RING_VA as *const u32).add(i)) }
}

fn ring_byte(i: usize) -> u8 {
    // SAFETY: as `ring_word`; the byte log lives inside the same page.
    unsafe { core::ptr::read_volatile((RING_VA as *const u8).add(RING_BYTES_OFFSET + i)) }
}

#[repr(C)]
struct MetaV2 {
    status: u64,
    opcode: u16,
    flags: u16,
    payload_len: u32,
    cap_id: u64,
    recv_meta_flags: u64,
    sender_tid: u64,
}

enum Recv {
    Message { label: u16, payload_len: u32 },
    Empty,
    TimedOut,
    Error,
}

/// One NR 5 with a recv-v2 metadata target, issued directly so a polling loop does not emit a log
/// line per probe. `timeout == 0` is the non-blocking probe.
fn recv_timeout(cap: u32, timeout: u64) -> Recv {
    let mut payload = [0u8; 64];
    let mut meta = MetaV2 {
        status: u64::MAX,
        opcode: 0,
        flags: 0,
        payload_len: 0,
        cap_id: u64::MAX,
        recv_meta_flags: 0,
        sender_tid: 0,
    };
    let mut a0 = cap as usize;
    let mut a1 = payload.as_mut_ptr() as usize;
    let mut a2 = payload.len();
    let mut a3 = timeout as usize;
    let mut a4 = (&mut meta as *mut MetaV2) as usize;
    let mut a5 = core::mem::size_of::<MetaV2>();
    // SAFETY: the kernel's RISC-V syscall ABI (a7 = number, a0..a5 = arguments); every argument
    // register is declared clobbered because the kernel may write any of them.
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") a0,
            inlateout("a1") a1,
            inlateout("a2") a2,
            inlateout("a3") a3,
            inlateout("a4") a4,
            inlateout("a5") a5,
            in("a7") NR_IPC_RECV_TIMEOUT,
            options(nostack),
        );
    }
    // SAFETY: the kernel's AArch64 syscall ABI (x8 = number, x0..x5 = arguments), clobbers as
    // above.
    //
    // QEMU-IRQ2 — every SIMD register is declared clobbered as well. AArch64 YARM keeps no per-task
    // FP/SIMD state: a receive that BLOCKS is resumed with whatever `q0..q31` the resuming vector
    // frame holds — for the idle-boundary return, the kernel's idle context. The receiver must
    // not park a live value there across a blocking receive, and this is what stops the compiler
    // from doing so. (The loss itself is measured, not hidden: see `park_measuring_simd`.)
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "svc #0",
            inlateout("x0") a0,
            inlateout("x1") a1,
            inlateout("x2") a2,
            inlateout("x3") a3,
            inlateout("x4") a4,
            inlateout("x5") a5,
            in("x8") NR_IPC_RECV_TIMEOUT,
            out("v8") _,
            out("v9") _,
            out("v10") _,
            out("v11") _,
            out("v12") _,
            out("v13") _,
            out("v14") _,
            out("v15") _,
            clobber_abi("C"),
            options(nostack),
        );
    }
    // SAFETY: the kernel's x86_64 SYSCALL ABI (rax = number; rdi, rsi, rdx, r10, r8, r9 =
    // arguments; rax/r8/rdx/rcx return, rcx carrying the error; SYSCALL itself clobbers rcx/r11).
    // `clobber_abi("C")` also declares every XMM register clobbered: the x86_64 kernel is built
    // with SSE and its entries save GPRs only, so no user XMM value survives a kernel entry
    // (QEMU-IRQ3; measured, not relied on).
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let err: usize;
        core::arch::asm!(
            "syscall",
            inlateout("rax") NR_IPC_RECV_TIMEOUT => _,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") err,
            clobber_abi("C"),
            options(nostack),
        );
        a0 = err;
    }
    let _ = (a1, a2, a3, a4, a5);
    // SAFETY: the kernel wrote `meta` through the pointer handed to it.
    let status = unsafe { core::ptr::read_volatile(&meta.status) };
    if status != u64::MAX {
        return Recv::Message {
            label: meta.opcode,
            payload_len: meta.payload_len,
        };
    }
    match a0 {
        ERR_WOULD_BLOCK => Recv::Empty,
        ERR_TIMED_OUT => Recv::TimedOut,
        _ => Recv::Error,
    }
}

/// QEMU-IRQ2 — the idle item's park, with four SIMD sentinels (`d0`, `d1`, `d16`, `d17`) carried
/// across the blocking receive in the SAME asm block. Returns the park's outcome and the mask of
/// sentinels that did NOT come back (bits 0..3 in that order). This is a MEASUREMENT of the resume path,
/// not of the interrupt path: the interrupt lands on the kernel's idle loop, and the receiver
/// resumes later through the timer's idle-boundary user return.
#[cfg(target_arch = "aarch64")]
fn park_measuring_simd(cap: u32, timeout: u64, seed: u64) -> (Recv, u32) {
    let mut payload = [0u8; 64];
    let mut meta = MetaV2 {
        status: u64::MAX,
        opcode: 0,
        flags: 0,
        payload_len: 0,
        cap_id: u64::MAX,
        recv_meta_flags: 0,
        sender_tid: 0,
    };
    let mut a0 = cap as usize;
    let vs = [
        seed ^ 0x51D0_0001,
        seed ^ 0x51D0_0002,
        seed ^ 0x51D0_0003,
        seed ^ 0x51D0_0004,
    ];
    let (mut v0, mut v1, mut v2, mut v3) = (vs[0], vs[1], vs[2], vs[3]);
    // SAFETY: NR 5 as in `recv_timeout`, with d0/d1/d16/d17 carried in and read back out.
    unsafe {
        core::arch::asm!(
            "svc #0",
            inlateout("x0") a0,
            inlateout("x1") payload.as_mut_ptr() as usize => _,
            inlateout("x2") payload.len() => _,
            inlateout("x3") timeout as usize => _,
            inlateout("x4") (&mut meta as *mut MetaV2) as usize => _,
            inlateout("x5") core::mem::size_of::<MetaV2>() => _,
            in("x8") NR_IPC_RECV_TIMEOUT,
            inout("v0") v0,
            inout("v1") v1,
            inout("v16") v2,
            inout("v17") v3,
            out("v8") _,
            out("v9") _,
            out("v10") _,
            out("v11") _,
            out("v12") _,
            out("v13") _,
            out("v14") _,
            out("v15") _,
            clobber_abi("C"),
            options(nostack),
        );
    }
    let mut mask = 0u32;
    for (i, (g, want)) in [v0, v1, v2, v3].iter().zip(vs.iter()).enumerate() {
        if g != want {
            mask |= 1 << i;
        }
    }
    // SAFETY: the kernel wrote `meta` through the pointer handed to it.
    let status = unsafe { core::ptr::read_volatile(&meta.status) };
    let outcome = if status != u64::MAX {
        Recv::Message {
            label: meta.opcode,
            payload_len: meta.payload_len,
        }
    } else {
        match a0 {
            ERR_WOULD_BLOCK => Recv::Empty,
            ERR_TIMED_OUT => Recv::TimedOut,
            _ => Recv::Error,
        }
    };
    (outcome, mask)
}

/// QEMU-IRQ3 — the x86_64 idle item's park with two XMM sentinels (`xmm8`, `xmm9`) carried across
/// the blocking receive in the same asm block. Observational, like the AArch64 measurement: bits
/// 0..1 set for sentinels that did not come back.
#[cfg(target_arch = "x86_64")]
fn park_measuring_simd(cap: u32, timeout: u64, seed: u64) -> (Recv, u32) {
    let mut payload = [0u8; 64];
    let mut meta = MetaV2 {
        status: u64::MAX,
        opcode: 0,
        flags: 0,
        payload_len: 0,
        cap_id: u64::MAX,
        recv_meta_flags: 0,
        sender_tid: 0,
    };
    let vs = [seed ^ 0x51D0_0001, seed ^ 0x51D0_0002];
    let (mut v0, mut v1) = (vs[0], vs[1]);
    let err: usize;
    // SAFETY: NR 5 as in `recv_timeout`, with xmm8/xmm9 carried in and read back out.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") NR_IPC_RECV_TIMEOUT => _,
            in("rdi") cap as usize,
            in("rsi") payload.as_mut_ptr() as usize,
            in("rdx") payload.len(),
            in("r10") timeout as usize,
            in("r8") (&mut meta as *mut MetaV2) as usize,
            in("r9") core::mem::size_of::<MetaV2>(),
            lateout("rcx") err,
            inout("xmm8") v0,
            inout("xmm9") v1,
            clobber_abi("C"),
            options(nostack),
        );
    }
    let mask = u32::from(v0 != vs[0]) | (u32::from(v1 != vs[1]) << 1);
    // SAFETY: the kernel wrote `meta` through the pointer handed to it.
    let status = unsafe { core::ptr::read_volatile(&meta.status) };
    let outcome = if status != u64::MAX {
        Recv::Message {
            label: meta.opcode,
            payload_len: meta.payload_len,
        }
    } else {
        match err {
            ERR_WOULD_BLOCK => Recv::Empty,
            ERR_TIMED_OUT => Recv::TimedOut,
            _ => Recv::Error,
        }
    };
    (outcome, mask)
}

/// Spin in U-mode with six sentinel registers live across the whole loop. Returns the mask of
/// registers whose value did NOT survive — `0` means the register file came back intact from every
/// interrupt that landed inside the loop.
#[inline(never)]
fn spin_checking_registers(iters: u64, seed: u64) -> u32 {
    let s = [
        seed,
        seed ^ 0x1111_1111,
        seed ^ 0x2222_2222,
        seed ^ 0x3333_3333,
        seed ^ 0x4444_4444,
        seed ^ 0x5555_5555,
    ];
    let (mut r0, mut r1, mut r2, mut r3, mut r4, mut r5) = (s[0], s[1], s[2], s[3], s[4], s[5]);
    // SAFETY: a pure register loop; no memory is touched.
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            "2:",
            "addi {n}, {n}, -1",
            "bnez {n}, 2b",
            n = inout(reg) iters => _,
            inout("t3") r0,
            inout("t4") r1,
            inout("t5") r2,
            inout("t6") r3,
            inout("a6") r4,
            inout("a7") r5,
            options(nomem, nostack),
        );
    }
    // SAFETY: as above. x9..x14 are AAPCS64 temporaries; x18 (TLS), x19 (LLVM base pointer),
    // x29 and x30 are not the instrument's to use.
    // QEMU-IRQ2: four SIMD sentinels as well (d0, d1, d16, d17; mask bits 6..9). An interrupt
    // taken from EL0 saves and restores q0..q31 in its vector frame, and the kernel's own copy
    // routines use q0/q1, so these must survive every interrupt in the loop.
    #[cfg(target_arch = "aarch64")]
    let vs = [
        seed ^ 0x51D0_0006,
        seed ^ 0x51D0_0007,
        seed ^ 0x51D0_0008,
        seed ^ 0x51D0_0009,
    ];
    #[cfg(target_arch = "aarch64")]
    let (mut v0, mut v1, mut v2, mut v3) = (vs[0], vs[1], vs[2], vs[3]);
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "2:",
            "subs {n}, {n}, #1",
            "b.ne 2b",
            n = inout(reg) iters => _,
            inout("x9") r0,
            inout("x10") r1,
            inout("x11") r2,
            inout("x12") r3,
            inout("x13") r4,
            inout("x14") r5,
            inout("v0") v0,
            inout("v1") v1,
            inout("v16") v2,
            inout("v17") v3,
            options(nomem, nostack),
        );
    }
    // QEMU-IRQ3 — x86_64: six GPR sentinels (r12..r15, r9, r10), the carry flag (set by `stc`
    // before the loop; `dec` leaves CF alone, so it must still be set after — bit 6), and two XMM
    // sentinels (xmm8, xmm9) that are OBSERVATIONAL only (bits 16..17): the x86_64 kernel does not
    // preserve user XMM state across any entry.
    #[cfg(target_arch = "x86_64")]
    let xs = [seed ^ 0x51D0_0006, seed ^ 0x51D0_0007];
    #[cfg(target_arch = "x86_64")]
    let (mut x0, mut x1) = (xs[0], xs[1]);
    #[cfg(target_arch = "x86_64")]
    let cf: u64;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "stc",
            "2:",
            "dec {n}",
            "jnz 2b",
            "setc {cf:l}",
            n = inout(reg) iters => _,
            cf = out(reg) cf,
            inout("r12") r0,
            inout("r13") r1,
            inout("r14") r2,
            inout("r15") r3,
            inout("r9") r4,
            inout("r10") r5,
            inout("xmm8") x0,
            inout("xmm9") x1,
            options(nomem, nostack),
        );
    }
    let got = [r0, r1, r2, r3, r4, r5];
    let mut mask = 0u32;
    for (i, (g, want)) in got.iter().zip(s.iter()).enumerate() {
        if g != want {
            mask |= 1 << i;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        for (i, (g, want)) in [v0, v1, v2, v3].iter().zip(vs.iter()).enumerate() {
            if g != want {
                mask |= 1 << (6 + i);
            }
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if cf & 0xff != 1 {
            mask |= 1 << 6;
        }
        mask |= (u32::from(x0 != xs[0]) << 16) | (u32::from(x1 != xs[1]) << 17);
    }
    mask
}

const NR_DEBUG_LOG: usize = 15;

struct LineBuf {
    buf: [u8; 96],
    len: usize,
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Announce a USER item and wait for it with the SAME six sentinel registers live across both.
///
/// The host injects the moment it reads the READY line, which it can only read once this task's
/// `DebugLog` has put it on the wire, so the interrupt is typically already pending when that
/// syscall returns and is taken on the very next U-mode instruction. Issuing the READY `ecall`
/// from inside the sentinel block puts that instruction — and the spin after it — under the
/// register check, instead of in the log formatter. Returns the mask of registers that did not
/// survive the syscall, the interrupt(s) and the spin.
#[inline(never)]
fn announce_ready_and_spin(seq: u32, expect: u8, iters: u64, seed: u64) -> u32 {
    let mut line = LineBuf {
        buf: [0u8; 96],
        len: 0,
    };
    let _ = core::fmt::write(
        &mut line,
        format_args!(
            "IRQ1_UART_READY seq={} mode=user expect=0x{:02x}",
            seq, expect
        ),
    );
    let s = [
        seed,
        seed ^ 0x1111_1111,
        seed ^ 0x2222_2222,
        seed ^ 0x3333_3333,
        seed ^ 0x4444_4444,
        seed ^ 0x5555_5555,
    ];
    let (mut r0, mut r1, mut r2, mut r3, mut r4, mut r5) = (s[0], s[1], s[2], s[3], s[4], s[5]);
    // SAFETY: `DebugLog` (a7 = 15, a0/a1 = the line) followed by a pure register loop. Every
    // argument register is declared clobbered; the sentinels sit in registers the syscall ABI
    // does not use.
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            "ecall",
            "2:",
            "addi s4, s4, -1",
            "bnez s4, 2b",
            inlateout("a0") line.buf.as_ptr() as usize => _,
            inlateout("a1") line.len => _,
            inlateout("a2") 0usize => _,
            inlateout("a3") 0usize => _,
            inlateout("a4") 0usize => _,
            inlateout("a5") 0usize => _,
            in("a7") NR_DEBUG_LOG,
            inout("s4") iters => _,
            inout("t3") r0,
            inout("t4") r1,
            inout("t5") r2,
            inout("t6") r3,
            inout("s2") r4,
            inout("s3") r5,
            options(nostack),
        );
    }
    // SAFETY: `DebugLog` (x8 = 15, x0/x1 = the line) followed by a pure register loop; the
    // sentinels sit in x9..x14, d0/d1/d16/d17 and the count in x15, none of which the syscall ABI
    // uses.
    #[cfg(target_arch = "aarch64")]
    let vs = [
        seed ^ 0x51D0_0006,
        seed ^ 0x51D0_0007,
        seed ^ 0x51D0_0008,
        seed ^ 0x51D0_0009,
    ];
    #[cfg(target_arch = "aarch64")]
    let (mut v0, mut v1, mut v2, mut v3) = (vs[0], vs[1], vs[2], vs[3]);
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "svc #0",
            "2:",
            "subs x15, x15, #1",
            "b.ne 2b",
            inlateout("x0") line.buf.as_ptr() as usize => _,
            inlateout("x1") line.len => _,
            inlateout("x2") 0usize => _,
            inlateout("x3") 0usize => _,
            inlateout("x4") 0usize => _,
            inlateout("x5") 0usize => _,
            in("x8") NR_DEBUG_LOG,
            inout("x15") iters => _,
            inout("x9") r0,
            inout("x10") r1,
            inout("x11") r2,
            inout("x12") r3,
            inout("x13") r4,
            inout("x14") r5,
            inout("v0") v0,
            inout("v1") v1,
            inout("v16") v2,
            inout("v17") v3,
            options(nostack),
        );
    }
    // SAFETY: `DebugLog` (rax = 15, rdi/rsi = the line) through SYSCALL, then the same checked loop
    // as `spin_checking_registers`: r12..r15/r9/r10 GPR sentinels, CF set by `stc` AFTER the
    // syscall (whose return resets RFLAGS) and required still set after the loop, xmm8/xmm9
    // observational. SYSCALL clobbers rcx/r11; the kernel writes rax/r8/rdx/rcx. Every allocatable
    // GPR is named here, so the count is an immediate (`SPIN_ITERS`, the only value ever passed).
    #[cfg(target_arch = "x86_64")]
    let _ = iters;
    #[cfg(target_arch = "x86_64")]
    let xs = [seed ^ 0x51D0_0006, seed ^ 0x51D0_0007];
    #[cfg(target_arch = "x86_64")]
    let (mut x0, mut x1) = (xs[0], xs[1]);
    #[cfg(target_arch = "x86_64")]
    let cf: u64;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "syscall",
            "mov rcx, {n}",
            "stc",
            "2:",
            "dec rcx",
            "jnz 2b",
            "setc al",
            n = const SPIN_ITERS,
            inlateout("rax") NR_DEBUG_LOG => cf,
            in("rdi") line.buf.as_ptr() as usize,
            in("rsi") line.len,
            lateout("rdx") _,
            lateout("rcx") _,
            lateout("r8") _,
            lateout("r11") _,
            inout("r12") r0,
            inout("r13") r1,
            inout("r14") r2,
            inout("r15") r3,
            inout("r9") r4,
            inout("r10") r5,
            inout("xmm8") x0,
            inout("xmm9") x1,
            options(nostack),
        );
    }
    let got = [r0, r1, r2, r3, r4, r5];
    let mut mask = 0u32;
    for (i, (g, want)) in got.iter().zip(s.iter()).enumerate() {
        if g != want {
            mask |= 1 << i;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        for (i, (g, want)) in [v0, v1, v2, v3].iter().zip(vs.iter()).enumerate() {
            if g != want {
                mask |= 1 << (6 + i);
            }
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if cf & 0xff != 1 {
            mask |= 1 << 6;
        }
        mask |= (u32::from(x0 != xs[0]) << 16) | (u32::from(x1 != xs[1]) << 17);
    }
    mask
}

static ISO_CHILD_STARTED: AtomicU32 = AtomicU32::new(0);
static ISO_CHILD_READ_RETURNED: AtomicU32 = AtomicU32::new(0);
static mut ISO_CHILD_STACK: [u8; 1024] = [0u8; 1024];
static mut ISO_CHILD_TLS: [u8; 256] = [0u8; 256];

/// The disposable isolation probe: a user-mode LOAD from a kernel-only device address (RISC-V: the
/// claim register's window address; AArch64: the PL011 flag register). The leaf carries no USER
/// bit, so the load must fault before the device is touched. Never returns: the fault terminates
/// this thread, and nothing else.
extern "C" fn isolation_child_body() -> ! {
    ISO_CHILD_STARTED.store(1, Relaxed);
    // SAFETY: deliberately touching a kernel-only mapping; the fault is the expected outcome.
    let v = unsafe { core::ptr::read_volatile(ISOLATION_LOAD_VA as *const u32) };
    ISO_CHILD_READ_RETURNED.store(1 | (v << 1), Relaxed);
    loop {
        let _ = yarm_user_rt::syscall::yield_now();
    }
}

/// Isolation, from userspace's side: (1) a U-mode load from the window faults in a disposable
/// child; (2) the window cannot be claimed by a user mapping (`VmAnonMap` over it is refused).
fn isolation_probes(park: u32) -> (u32, u32, bool) {
    // SAFETY: an anonymous-map request the kernel must refuse; nothing is mapped on refusal.
    let anon = unsafe { yarm_user_rt::syscall::vm_anon_map(ISOLATION_MAP_VA, 4096, 1) };
    let anon_refused = anon.is_err();
    let stack_top = (core::ptr::addr_of_mut!(ISO_CHILD_STACK) as usize + 1024) & !0xF;
    let tls_base = core::ptr::addr_of_mut!(ISO_CHILD_TLS) as usize;
    // SAFETY: a valid `extern "C" fn() -> !`; the static stack and TLS outlive the thread.
    let child = unsafe {
        yarm_user_rt::syscall::spawn_thread(
            tls_base,
            stack_top,
            isolation_child_body as *const () as usize,
        )
    };
    let child_tid = child.map(|t| t as u32).unwrap_or(0);
    // Park so the scheduler runs the child and it takes its fault.
    let mut rounds = 0;
    while ISO_CHILD_STARTED.load(Relaxed) == 0 && rounds < 50 {
        let _ = recv_timeout(park, 2);
        rounds += 1;
    }
    for _ in 0..5 {
        let _ = recv_timeout(park, 2);
    }
    (
        child_tid,
        ISO_CHILD_READ_RETURNED.load(Relaxed),
        anon_refused,
    )
}

pub(super) fn run_once() {
    let notif = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::runtime::STARTUP_SLOT_SERVICE_EXTRA_CAP_0,
    )
    .unwrap_or(0) as u32;
    let park = yarm_user_rt::runtime::startup_arg_slot(
        yarm_user_rt::runtime::STARTUP_SLOT_SERVICE_EXTRA_CAP_1,
    )
    .unwrap_or(0) as u32;
    if notif == 0 || park == 0 || ring_word(W_MAGIC) != RING_MAGIC {
        yarm_user_rt::user_log!(
            "IRQ1_UART_WITNESS step=provision result=missing notif={} park={}",
            notif,
            park
        );
        RESULT.store(0xF0, Relaxed);
        return;
    }
    yarm_user_rt::user_log!(
        "IRQ1_UART_WITNESS_BEGIN items={} notif_cap={} park_cap={} ring_va=0x{:x} spin_fn=0x{:x}",
        ITEMS,
        notif,
        park,
        RING_VA,
        spin_checking_registers as *const () as usize
    );

    // Wait for the kernel to enable the source. The enable happens at the first idle safe point,
    // so parking is also what gets it there.
    let mut arm_rounds = 0u32;
    while ring_word(W_ARMED) == 0 {
        let _ = recv_timeout(park, 2);
        arm_rounds += 1;
        if arm_rounds > MAX_IDLE_ROUNDS {
            yarm_user_rt::user_log!("IRQ1_UART_WITNESS step=arm result=timeout");
            RESULT.store(0xF1, Relaxed);
            return;
        }
    }
    // The line the kernel bound (read from the DTB), as the kernel published it: every label must
    // name it.
    let bound_line = ring_word(W_LINE) as u16;
    yarm_user_rt::user_log!(
        "IRQ1_UART_ARMED rounds={} expect_label={}",
        arm_rounds,
        bound_line
    );

    let (mut received, mut exact_once, mut label_ok, mut data_ok) = (0u32, 0u32, 0u32, 0u32);
    let (mut idle_items, mut user_items, mut regs_bad, mut dup, mut errors) =
        (0u32, 0u32, 0u32, 0u32, 0u32);
    for seq in 1..=ITEMS {
        let idle = seq % 2 == 1;
        let expect = BYTE_BASE + seq as u8;
        let mut rounds = 0u32;
        let mut bad_mask = 0u32;
        // QEMU-IRQ2: SIMD sentinels carried across an idle item's park (AArch64 only; always 0
        // elsewhere). Observational — it names the resume path's FP/SIMD loss, it is not graded.
        #[allow(unused_mut)]
        let mut idle_simd_mask = 0u32;
        if idle {
            yarm_user_rt::user_log!(
                "IRQ1_UART_READY seq={} mode=idle expect=0x{:02x}",
                seq,
                expect
            );
        } else {
            // Round 1 of a USER item: announce and spin under one register check.
            let seed = 0x5EED_0000_0000u64 | ((seq as u64) << 16);
            bad_mask |= announce_ready_and_spin(seq, expect, SPIN_ITERS, seed);
        }
        let got = loop {
            rounds += 1;
            if idle {
                if rounds > MAX_IDLE_ROUNDS {
                    break None;
                }
                #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
                let parked = {
                    let seed = 0x5EED_0000_0000u64 | ((seq as u64) << 16) | rounds as u64;
                    let (r, m) = park_measuring_simd(park, IDLE_PARK_TICKS, seed);
                    idle_simd_mask |= m;
                    r
                };
                #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
                let parked = recv_timeout(park, IDLE_PARK_TICKS);
                match parked {
                    Recv::Message { .. } => dup += 1,
                    Recv::Error => errors += 1,
                    Recv::TimedOut | Recv::Empty => {}
                }
            } else {
                if rounds > MAX_USER_ROUNDS {
                    break None;
                }
                let seed = 0x5EED_0000_0000u64 | ((seq as u64) << 16) | rounds as u64;
                bad_mask |= spin_checking_registers(SPIN_ITERS, seed);
            }
            match recv_timeout(notif, 0) {
                Recv::Message { label, payload_len } => break Some((label, payload_len)),
                Recv::Error => errors += 1,
                Recv::Empty | Recv::TimedOut => {}
            }
        };
        let Some((label, payload_len)) = got else {
            yarm_user_rt::user_log!(
                "IRQ1_UART_RECV seq={} result=timeout rounds={} bytes={} claims={} completions={}",
                seq,
                rounds,
                ring_word(W_BYTES),
                ring_word(W_CLAIMS),
                ring_word(W_COMPLETIONS)
            );
            break;
        };
        received += 1;
        // Exactly one notification for this item: an immediate second probe must be empty.
        let once = matches!(recv_timeout(notif, 0), Recv::Empty);
        if once {
            exact_once += 1;
        } else {
            dup += 1;
        }
        if label == bound_line {
            label_ok += 1;
        }
        let bytes = ring_word(W_BYTES);
        let byte = ring_byte(seq as usize - 1);
        let data = bytes == seq && byte == expect;
        if data {
            data_ok += 1;
        }
        if idle {
            idle_items += 1;
        } else {
            user_items += 1;
            // Bits 0..15 are graded (GPRs, flags, and AArch64's saved-and-restored SIMD); bits 16+
            // are x86_64's observational XMM sentinels, reported separately below.
            if bad_mask & 0xffff != 0 {
                regs_bad += 1;
            }
        }
        yarm_user_rt::user_log!(
            "IRQ1_UART_RECV seq={} mode={} label={} payload_len={} once={} byte=0x{:02x} bytes={} data_ok={} rounds={} regs_mask=0x{:x} claims={} completions={} idle_resume_simd_mask=0x{:x}",
            seq,
            if idle { "idle" } else { "user" },
            label,
            payload_len,
            once as u8,
            byte,
            bytes,
            data as u8,
            rounds,
            bad_mask & 0xffff,
            ring_word(W_CLAIMS),
            ring_word(W_COMPLETIONS),
            idle_simd_mask
        );
        // QEMU-IRQ3: the x86_64 XMM sentinels, on a line of their own (the RECV line is at the
        // user-log length limit). Observational, never graded by the receiver.
        #[cfg(target_arch = "x86_64")]
        yarm_user_rt::user_log!(
            "IRQ1_UART_SIMD seq={} mode={} user_simd_mask=0x{:x} idle_resume_simd_mask=0x{:x}",
            seq,
            if idle { "idle" } else { "user" },
            bad_mask >> 16,
            idle_simd_mask
        );
    }

    // ── After the source is off: timer and service progress, and silence ──────────────────────
    let disabled = ring_word(W_DISABLED);
    let (bytes_before, claims_before) = (ring_word(W_BYTES), ring_word(W_CLAIMS));
    yarm_user_rt::user_log!(
        "IRQ1_UART_POSTDISABLE_READY disabled={} bytes={} claims={}",
        disabled,
        bytes_before,
        claims_before
    );
    let (mut post_timeouts, mut post_notifications) = (0u32, 0u32);
    for _ in 0..POST_ROUNDS {
        if matches!(recv_timeout(park, POST_PARK_TICKS), Recv::TimedOut) {
            post_timeouts += 1;
        }
        if matches!(recv_timeout(notif, 0), Recv::Message { .. }) {
            post_notifications += 1;
        }
    }
    let post_quiet = post_notifications == 0
        && ring_word(W_BYTES) == bytes_before
        && ring_word(W_CLAIMS) == claims_before;
    let claims = ring_word(W_CLAIMS);
    let completions = ring_word(W_COMPLETIONS);

    let (iso_child, iso_returned, anon_refused) = isolation_probes(park);
    let isolated = iso_child != 0 && iso_returned == 0 && anon_refused;
    yarm_user_rt::user_log!(
        "IRQ1_UART_ISOLATION child_tid={} window_va=0x{:x} load_returned={} anon_map_over_window_refused={} result={}",
        iso_child,
        ISOLATION_LOAD_VA,
        iso_returned,
        anon_refused as u8,
        if isolated { "ok" } else { "fail" }
    );

    let all = received == ITEMS
        && exact_once == ITEMS
        && label_ok == ITEMS
        && data_ok == ITEMS
        && regs_bad == 0
        && dup == 0
        && errors == 0
        && claims == completions
        && disabled == 1
        && post_timeouts == POST_ROUNDS
        && post_quiet
        && isolated;
    RESULT.store(u32::from(all), Relaxed);
    yarm_user_rt::user_log!(
        "IRQ1_UART_WITNESS_COUNTS claims={} completions={} empty_drains={} idle_origin={} user_origin={} disabled={} post_rounds={} post_timeouts={} post_notifications={} post_quiet={}",
        claims,
        completions,
        ring_word(W_EMPTY),
        ring_word(W_IDLE),
        ring_word(W_USER),
        disabled,
        POST_ROUNDS,
        post_timeouts,
        post_notifications,
        post_quiet as u8
    );
    yarm_user_rt::user_log!(
        "IRQ1_UART_WITNESS items={} received={} exact_once={} label_ok={} data_ok={} idle_items={} user_items={} regs_bad={} dup={} errors={} isolated={} result={}",
        ITEMS,
        received,
        exact_once,
        label_ok,
        data_ok,
        idle_items,
        user_items,
        regs_bad,
        dup,
        errors,
        isolated as u8,
        if all { "ok" } else { "fail" }
    );
}
