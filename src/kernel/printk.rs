// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(not(feature = "hosted-dev"))]
use crate::kernel::lock::SpinLockIrq;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};

pub const PRB_SLOTS: usize = 128;
pub const PRB_MSG_MAX: usize = 192;
const _: () = assert!(
    PRB_SLOTS.is_power_of_two(),
    "PRB_SLOTS must be a power of two"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LogLevel {
    Emerg = 0,
    Alert = 1,
    Crit = 2,
    Err = 3,
    Warn = 4,
    Notice = 5,
    Info = 6,
    Debug = 7,
}

impl LogLevel {
    /// Stage 108: public `from_u8` for the `yarm.loglevel=` boot-cmdline knob.
    /// Same clamping as the private decoder (8+ ⇒ Debug).
    pub const fn from_u8_public(raw: u8) -> Self {
        Self::from_u8(raw)
    }

    const fn from_u8(raw: u8) -> Self {
        match raw {
            0 => Self::Emerg,
            1 => Self::Alert,
            2 => Self::Crit,
            3 => Self::Err,
            4 => Self::Warn,
            5 => Self::Notice,
            6 => Self::Info,
            _ => Self::Debug,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PrintkContext {
    Normal = 0,
    Irq = 1,
    Nmi = 2,
}

impl PrintkContext {
    const fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::Irq,
            2 => Self::Nmi,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintkRecord {
    pub seq: u64,
    pub level: LogLevel,
    pub context: PrintkContext,
    pub len: usize,
    pub message: [u8; PRB_MSG_MAX],
}

impl PrintkRecord {
    pub const fn empty() -> Self {
        Self {
            seq: 0,
            level: LogLevel::Debug,
            context: PrintkContext::Normal,
            len: 0,
            message: [0; PRB_MSG_MAX],
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.message[..self.len]).unwrap_or("<invalid-utf8>")
    }
}

struct PrbSlot {
    seq: AtomicU64,
    len: AtomicU16,
    level: AtomicU8,
    context: AtomicU8,
    committed: AtomicBool,
    bytes: [AtomicU8; PRB_MSG_MAX],
}

impl PrbSlot {
    const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            len: AtomicU16::new(0),
            level: AtomicU8::new(LogLevel::Debug as u8),
            context: AtomicU8::new(PrintkContext::Normal as u8),
            committed: AtomicBool::new(false),
            bytes: [const { AtomicU8::new(0) }; PRB_MSG_MAX],
        }
    }

    fn write(&self, seq: u64, level: LogLevel, context: PrintkContext, msg: &[u8]) {
        self.committed.store(false, Ordering::Release);
        let len = core::cmp::min(msg.len(), PRB_MSG_MAX);
        let mut i = 0usize;
        while i < len {
            self.bytes[i].store(msg[i], Ordering::Relaxed);
            i += 1;
        }
        self.len.store(len as u16, Ordering::Relaxed);
        self.level.store(level as u8, Ordering::Relaxed);
        self.context.store(context as u8, Ordering::Relaxed);
        self.seq.store(seq, Ordering::Release);
        self.committed.store(true, Ordering::Release);
    }

    fn read_if_seq(&self, expected_seq: u64) -> Option<PrintkRecord> {
        if !self.committed.load(Ordering::Acquire) {
            return None;
        }
        let seq0 = self.seq.load(Ordering::Acquire);
        if seq0 != expected_seq {
            return None;
        }
        let len = self.len.load(Ordering::Relaxed) as usize;
        let mut out = PrintkRecord::empty();
        out.seq = seq0;
        out.len = core::cmp::min(len, PRB_MSG_MAX);
        out.level = LogLevel::from_u8(self.level.load(Ordering::Relaxed));
        out.context = PrintkContext::from_u8(self.context.load(Ordering::Relaxed));
        let mut i = 0usize;
        while i < out.len {
            out.message[i] = self.bytes[i].load(Ordering::Relaxed);
            i += 1;
        }
        let seq1 = self.seq.load(Ordering::Acquire);
        if seq1 != seq0 || !self.committed.load(Ordering::Acquire) {
            return None;
        }
        Some(out)
    }
}

struct PrintkRing {
    next_seq: AtomicU64,
    dropped: AtomicU64,
    truncated: AtomicU64,
    slots: [PrbSlot; PRB_SLOTS],
    console_loglevel: AtomicU8,
    reader_seq: AtomicU64,
}

impl PrintkRing {
    const fn new() -> Self {
        Self {
            next_seq: AtomicU64::new(1),
            dropped: AtomicU64::new(0),
            truncated: AtomicU64::new(0),
            slots: [const { PrbSlot::new() }; PRB_SLOTS],
            console_loglevel: AtomicU8::new(LogLevel::Info as u8),
            reader_seq: AtomicU64::new(1),
        }
    }

    fn push(&self, level: LogLevel, context: PrintkContext, msg: &[u8]) {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        let idx = (seq as usize) & (PRB_SLOTS - 1);
        if msg.len() > PRB_MSG_MAX {
            self.truncated.fetch_add(1, Ordering::Relaxed);
        }
        self.slots[idx].write(seq, level, context, msg);
    }

    fn snapshot_latest(&self, out: &mut [PrintkRecord]) -> usize {
        let last = self.next_seq.load(Ordering::Acquire).saturating_sub(1);
        if last == 0 || out.is_empty() {
            return 0;
        }
        let start = last.saturating_sub(out.len() as u64).saturating_add(1);
        let mut written = 0usize;
        let mut seq = start;
        while seq <= last && written < out.len() {
            let idx = (seq as usize) & (PRB_SLOTS - 1);
            if let Some(rec) = self.slots[idx].read_if_seq(seq) {
                out[written] = rec;
                written += 1;
            }
            seq = seq.saturating_add(1);
        }
        written
    }

    /// Drain every record from `reader_seq` on, in sequence order.
    ///
    /// BL5d: a record that is RESERVED but not yet COMMITTED — another CPU has taken its sequence
    /// number and is still copying its bytes — is held, not skipped. The drain stops in front of
    /// it and leaves `reader_seq` there; its writer's own flush (every `yarm_log!` pushes and then
    /// flushes) emits it and everything queued behind it, in order. Skipping it instead advanced
    /// `reader_seq` past a record nothing would ever drain again: the line was silently lost, and
    /// an asynchronous one-shot marker of a COMPLETED transaction read as a transaction that never
    /// happened.
    ///
    /// A record is genuinely lost only when its slot has been reused: a later sequence number holds
    /// the slot, or has been reserved for it (the ring has lapped the record). That loss is counted
    /// in `dropped` and announced in the console stream, at the place it happened, by one
    /// `PRINTK_RECORDS_LOST` line per contiguous run — so a missing line always carries evidence of
    /// its own loss, and an unannounced absence means the record was never written.
    fn drain_for_console<F: FnMut(LogLevel, PrintkContext, &str)>(&self, mut sink: F) -> usize {
        let mut drained = 0usize;
        let threshold = self.console_loglevel.load(Ordering::Acquire);
        let mut seq = self.reader_seq.load(Ordering::Acquire);
        let last = self.next_seq.load(Ordering::Acquire).saturating_sub(1);
        // (first lost sequence, run length) of the run not yet announced.
        let mut lost: Option<(u64, u64)> = None;
        while seq <= last {
            let idx = (seq as usize) & (PRB_SLOTS - 1);
            if let Some(rec) = self.slots[idx].read_if_seq(seq) {
                self.announce_lost(&mut lost, &mut sink);
                if (rec.level as u8) <= threshold {
                    sink(rec.level, rec.context, rec.as_str());
                }
                drained += 1;
                seq = seq.saturating_add(1);
                continue;
            }
            let held = self.slots[idx].seq.load(Ordering::Acquire);
            let lapped =
                self.next_seq.load(Ordering::Acquire).saturating_sub(seq) > PRB_SLOTS as u64;
            if held > seq || lapped {
                self.dropped.fetch_add(1, Ordering::AcqRel);
                lost = Some(match lost {
                    Some((first, n)) => (first, n + 1),
                    None => (seq, 1),
                });
                seq = seq.saturating_add(1);
                continue;
            }
            // In flight: reserved, not yet committed. Hold it.
            break;
        }
        self.announce_lost(&mut lost, &mut sink);
        self.reader_seq.store(seq, Ordering::Release);
        drained
    }

    fn announce_lost<F: FnMut(LogLevel, PrintkContext, &str)>(
        &self,
        lost: &mut Option<(u64, u64)>,
        sink: &mut F,
    ) {
        if let Some((first, n)) = lost.take() {
            let mut sb = StackBuf::new();
            let _ = fmt::write(
                &mut sb,
                format_args!(
                    "PRINTK_RECORDS_LOST first_seq={} count={} total_dropped={}",
                    first,
                    n,
                    self.dropped.load(Ordering::Acquire)
                ),
            );
            if let Ok(line) = core::str::from_utf8(&sb.buf[..sb.len]) {
                sink(LogLevel::Err, PrintkContext::Normal, line);
            }
        }
    }
}

static PRINTK: PrintkRing = PrintkRing::new();
#[cfg(not(feature = "hosted-dev"))]
static PRINTK_DRAIN_LOCK: SpinLockIrq<()> = SpinLockIrq::new(());

pub fn printk_state_addr() -> usize {
    core::ptr::addr_of!(PRINTK) as usize
}

struct StackBuf {
    len: usize,
    buf: [u8; PRB_MSG_MAX],
}

impl StackBuf {
    const fn new() -> Self {
        Self {
            len: 0,
            buf: [0; PRB_MSG_MAX],
        }
    }
}

impl fmt::Write for StackBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let avail = PRB_MSG_MAX.saturating_sub(self.len);
        let copy = core::cmp::min(avail, bytes.len());
        if copy == 0 {
            return Ok(());
        }
        self.buf[self.len..self.len + copy].copy_from_slice(&bytes[..copy]);
        self.len += copy;
        Ok(())
    }
}

pub fn printk_args(level: LogLevel, context: PrintkContext, args: fmt::Arguments<'_>) {
    let mut sb = StackBuf::new();
    let _ = fmt::write(&mut sb, args);
    PRINTK.push(level, context, &sb.buf[..sb.len]);
}

pub fn printk_flush() -> usize {
    #[cfg(not(feature = "hosted-dev"))]
    {
        let _drain_guard = PRINTK_DRAIN_LOCK.lock();
        return threaded_drain_to(|_lvl, _ctx, msg| crate::arch::console::write_line(msg));
    }
    #[cfg(feature = "hosted-dev")]
    {
        0
    }
}

/// Stage 189D: emit a line to the console SYNCHRONOUSLY, bypassing the ring buffer.
/// Under concurrent multi-CPU logging the shared ring can overflow and DROP a pushed
/// message before it is flushed; a required proof marker must not be lost that way.
/// This formats into a stack buffer, takes the drain lock (serializing against
/// concurrent flushes so the line is not byte-interleaved), drains any pending ring
/// messages first to preserve ordering, then writes the line directly (the console
/// writer polls THR-empty per byte, so no byte-level drop). Use SPARINGLY — it blocks
/// on the UART. Intended for a handful of AP user-dispatch seal markers.
#[cfg(not(feature = "hosted-dev"))]
pub fn printk_emit_sync(args: fmt::Arguments<'_>) {
    let mut sb = StackBuf::new();
    let _ = fmt::write(&mut sb, args);
    let _drain_guard = PRINTK_DRAIN_LOCK.lock();
    let _ = threaded_drain_to(|_lvl, _ctx, msg| crate::arch::console::write_line(msg));
    if let Ok(line) = core::str::from_utf8(&sb.buf[..sb.len]) {
        crate::arch::console::write_line(line);
    }
}
#[cfg(feature = "hosted-dev")]
pub fn printk_emit_sync(_args: fmt::Arguments<'_>) {}

#[macro_export]
macro_rules! printk {
    ($lvl:expr, $ctx:expr, $($arg:tt)*) => {{
        $crate::kernel::printk::printk_args($lvl, $ctx, format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! pr_err {
    ($($arg:tt)*) => {{
        $crate::printk!($crate::kernel::printk::LogLevel::Err, $crate::kernel::printk::PrintkContext::Normal, $($arg)*);
    }};
}

#[macro_export]
macro_rules! pr_warn {
    ($($arg:tt)*) => {{
        $crate::printk!($crate::kernel::printk::LogLevel::Warn, $crate::kernel::printk::PrintkContext::Normal, $($arg)*);
    }};
}

#[macro_export]
macro_rules! pr_info {
    ($($arg:tt)*) => {{
        $crate::printk!($crate::kernel::printk::LogLevel::Info, $crate::kernel::printk::PrintkContext::Normal, $($arg)*);
    }};
}

#[macro_export]
macro_rules! pr_debug {
    ($($arg:tt)*) => {{
        $crate::printk!($crate::kernel::printk::LogLevel::Debug, $crate::kernel::printk::PrintkContext::Normal, $($arg)*);
    }};
}

pub fn set_console_loglevel(level: LogLevel) {
    PRINTK
        .console_loglevel
        .store(level as u8, Ordering::Release);
}

pub fn console_loglevel() -> LogLevel {
    LogLevel::from_u8(PRINTK.console_loglevel.load(Ordering::Acquire))
}

pub fn dropped_count() -> u64 {
    PRINTK.dropped.load(Ordering::Acquire)
}

pub fn truncated_count() -> u64 {
    PRINTK.truncated.load(Ordering::Acquire)
}

pub fn snapshot_latest(out: &mut [PrintkRecord]) -> usize {
    PRINTK.snapshot_latest(out)
}

pub fn threaded_drain_to<F: FnMut(LogLevel, PrintkContext, &str)>(sink: F) -> usize {
    PRINTK.drain_for_console(sink)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::string::String;
    use std::vec::Vec;

    #[test]
    fn printk_commits_to_prb() {
        printk_args(
            LogLevel::Info,
            PrintkContext::Normal,
            format_args!("hello {}", 7),
        );
        let mut recs = [PrintkRecord::empty(); 8];
        let n = snapshot_latest(&mut recs);
        assert!(n >= 1);
        let last = &recs[n - 1];
        assert!(last.as_str().contains("hello 7"));
    }

    #[test]
    fn nmi_context_logging_is_safe_and_recorded() {
        printk_args(
            LogLevel::Err,
            PrintkContext::Nmi,
            format_args!("nmi panic path"),
        );
        let mut recs = [PrintkRecord::empty(); 8];
        let n = snapshot_latest(&mut recs);
        assert!(n >= 1);
        assert_eq!(recs[n - 1].context, PrintkContext::Nmi);
    }

    #[test]
    fn console_loglevel_round_trips() {
        set_console_loglevel(LogLevel::Warn);
        assert_eq!(console_loglevel(), LogLevel::Warn);
        set_console_loglevel(LogLevel::Info);
    }

    // BL5d — the drain holds an in-flight record and announces a lost one.
    fn drain_all(ring: &PrintkRing) -> Vec<String> {
        let mut out = Vec::<String>::new();
        let _ = ring.drain_for_console(|_lvl, _ctx, msg| out.push(msg.into()));
        out
    }

    fn push(ring: &PrintkRing, msg: &str) {
        ring.push(LogLevel::Info, PrintkContext::Normal, msg.as_bytes());
    }

    #[test]
    fn bl5d_an_uncommitted_record_is_held_not_skipped() {
        let ring = std::boxed::Box::new(PrintkRing::new());
        push(&ring, "A");
        // Another CPU reserves the next sequence number and is still copying its bytes.
        let s = ring.next_seq.fetch_add(1, Ordering::AcqRel);
        push(&ring, "B");
        assert_eq!(
            drain_all(&ring),
            ["A"],
            "the drain stops in front of the in-flight record"
        );
        assert_eq!(ring.reader_seq.load(Ordering::Acquire), s);
        // The writer commits; its own flush then emits it and what queued behind it, in order.
        ring.slots[(s as usize) & (PRB_SLOTS - 1)].write(
            s,
            LogLevel::Info,
            PrintkContext::Normal,
            b"S",
        );
        assert_eq!(drain_all(&ring), ["S", "B"]);
        assert_eq!(ring.dropped.load(Ordering::Acquire), 0);
        assert!(drain_all(&ring).is_empty(), "nothing is emitted twice");
    }

    #[test]
    fn bl5d_an_overwritten_record_is_counted_and_announced_where_it_was_lost() {
        let ring = std::boxed::Box::new(PrintkRing::new());
        for i in 1..=(PRB_SLOTS + 2) {
            push(&ring, &std::format!("m{i}"));
        }
        let out = drain_all(&ring);
        assert_eq!(
            out[0],
            "PRINTK_RECORDS_LOST first_seq=1 count=2 total_dropped=2"
        );
        assert_eq!(out[1], "m3");
        assert_eq!(out.len(), 1 + PRB_SLOTS);
        assert_eq!(ring.dropped.load(Ordering::Acquire), 2);
    }

    #[test]
    fn bl5d_an_in_flight_record_the_ring_has_lapped_is_lost_not_waited_on() {
        let ring = std::boxed::Box::new(PrintkRing::new());
        let s = ring.next_seq.fetch_add(1, Ordering::AcqRel);
        for i in 0..PRB_SLOTS {
            push(&ring, &std::format!("n{i}"));
        }
        let out = drain_all(&ring);
        assert_eq!(
            out[0],
            std::format!("PRINTK_RECORDS_LOST first_seq={s} count=1 total_dropped=1")
        );
        assert_eq!(out.len(), 1 + PRB_SLOTS);
    }

    /// Writers on several threads race a draining thread; nothing laps. Every record must be
    /// emitted exactly once — none skipped while another thread was still writing it.
    #[test]
    fn bl5d_concurrent_writers_lose_nothing_the_ring_still_holds() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool as StdBool;
        const WRITERS: usize = 4;
        const EACH: usize = 30; // 120 < PRB_SLOTS: the ring never laps
        for _round in 0..200 {
            let ring = Arc::new(PrintkRing::new());
            let done = Arc::new(StdBool::new(false));
            let drainer = {
                let (ring, done) = (Arc::clone(&ring), Arc::clone(&done));
                std::thread::spawn(move || {
                    let mut out = Vec::<String>::new();
                    loop {
                        let finished = done.load(Ordering::Acquire);
                        let _ = ring.drain_for_console(|_l, _c, m| out.push(m.into()));
                        if finished {
                            break out;
                        }
                    }
                })
            };
            let writers: Vec<_> = (0..WRITERS)
                .map(|w| {
                    let ring = Arc::clone(&ring);
                    std::thread::spawn(move || {
                        for i in 0..EACH {
                            push(&ring, &std::format!("w{w}-{i}"));
                        }
                    })
                })
                .collect();
            for t in writers {
                t.join().unwrap();
            }
            done.store(true, Ordering::Release);
            let mut out = drainer.join().unwrap();
            out.extend(drain_all(&ring));
            assert_eq!(out.len(), WRITERS * EACH, "every record emitted: {out:?}");
            out.sort();
            out.dedup();
            assert_eq!(out.len(), WRITERS * EACH, "and each exactly once");
            assert_eq!(ring.dropped.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn threaded_drain_applies_console_loglevel() {
        set_console_loglevel(LogLevel::Warn);
        printk_args(
            LogLevel::Info,
            PrintkContext::Normal,
            format_args!("info-msg"),
        );
        printk_args(
            LogLevel::Err,
            PrintkContext::Normal,
            format_args!("err-msg"),
        );

        let mut out = Vec::<String>::new();
        let _ = threaded_drain_to(|_lvl, _ctx, msg| out.push(msg.into()));
        assert!(out.iter().any(|m| m.contains("err-msg")));
        assert!(!out.iter().any(|m| m.contains("info-msg")));
    }
}
