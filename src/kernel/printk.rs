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

    fn drain_for_console<F: FnMut(LogLevel, PrintkContext, &str)>(&self, mut sink: F) -> usize {
        let mut drained = 0usize;
        let threshold = self.console_loglevel.load(Ordering::Acquire);
        let mut seq = self.reader_seq.load(Ordering::Acquire);
        let last = self.next_seq.load(Ordering::Acquire).saturating_sub(1);
        while seq <= last {
            let idx = (seq as usize) & (PRB_SLOTS - 1);
            if let Some(rec) = self.slots[idx].read_if_seq(seq) {
                if (rec.level as u8) <= threshold {
                    sink(rec.level, rec.context, rec.as_str());
                }
                drained += 1;
            }
            seq = seq.saturating_add(1);
        }
        self.reader_seq.store(seq, Ordering::Release);
        drained
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
