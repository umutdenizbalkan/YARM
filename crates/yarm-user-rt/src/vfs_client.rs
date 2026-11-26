// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Reusable VFS client helpers for userspace tasks.
//!
//! These helpers abstract the IPC frame construction, call, and reply
//! drain pattern for VFS operations so callers never hand-build
//! `VFS_OP_STATX` / `VFS_OP_OPENAT` frames directly.
//!
//! Encoding helpers (`build_*_message`) are safe and return a fully
//! constructed [`crate::ipc::Message`] that can be inspected in tests
//! without live kernel endpoints.  The actual IPC helpers (`vfs_statx`,
//! `vfs_openat`, `vfs_read`, `vfs_close`) are `unsafe` because they invoke
//! the kernel via `ipc_call` + `ipc_recv_v2` (blocking receive).
//!
//! VFS-CORRECT1A: one transaction is NR 6 then, only if NR 6 succeeded, one blocking receive.
//! A send failure is returned at once and nothing is received. The reply must be a valid
//! completion (`yarm_ipc_abi::vfs_abi::validate_vfs_completion`) for the request it answers
//! before a byte is copied or an fd is returned; a READ copies exactly the validated count into
//! the front of the caller's slice and leaves the rest untouched. Nothing is retried. The
//! `*_with` forms take a [`VfsTransport`] so the transaction can be driven without a kernel.

use crate::ipc::Message;
use crate::syscall::{ReceivedMessage, SyscallError};
use yarm_ipc_abi::vfs_abi::{
    BulkReadArgs, FileGrantRoArgs, OpenAtInlinePath, ReadWriteArgs, StatxInlinePath,
    VFS_INLINE_READ_MAX, VFS_OP_CLOSE, VFS_OP_FILE_GRANT_RO, VFS_OP_OPENAT, VFS_OP_READ,
    VFS_OP_READ_BULK, VFS_OP_STATX, VfsCompletionExpect, VfsCompletionFault, VfsCompletionOutcome,
    VfsV1Args, validate_vfs_completion,
};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during a VFS client operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsClientError {
    /// Path is empty or exceeds the inline-path maximum (96 bytes).
    PathTooLong,
    /// IPC message construction failed (internal payload overflow).
    MessageFailed,
    /// NR 6 failed: nothing was delivered and no receive was attempted.
    Send(SyscallError),
    /// The receive on the reply endpoint failed.
    Recv(SyscallError),
    /// The receive on the reply endpoint returned no message.
    NoReply,
    /// The reply is not a valid completion for this request; nothing was copied or published.
    Malformed(VfsCompletionFault),
    /// The router or the backend reported this `VFS_STATUS_ERR_*`.
    Status(u32),
}

// ── Encoding helpers (safe, testable without live endpoints) ──────────────────

/// Build a `VFS_OP_STATX` [`Message`] for `path`.
///
/// `dirfd`, `flags`, and `mask_or_buf` are zeroed (suitable for a plain
/// path-stat without mount-relative lookup or mask filtering).
pub fn build_statx_message(path: &[u8]) -> Result<Message, VfsClientError> {
    let (buf, len) = StatxInlinePath {
        dirfd: 0,
        flags: 0,
        mask_or_buf: 0,
        path,
    }
    .encode()
    .ok_or(VfsClientError::PathTooLong)?;
    Message::with_header(0, VFS_OP_STATX, 0, None, &buf[..len])
        .map_err(|_| VfsClientError::MessageFailed)
}

/// Build a `VFS_OP_OPENAT` [`Message`] for `path` with the given `flags`.
///
/// `dirfd` and `mode` are zeroed (root-relative open, no creation mode).
pub fn build_openat_message(path: &[u8], flags: u64) -> Result<Message, VfsClientError> {
    let (buf, len) = OpenAtInlinePath {
        dirfd: 0,
        flags,
        mode: 0,
        path,
    }
    .encode()
    .ok_or(VfsClientError::PathTooLong)?;
    Message::with_header(0, VFS_OP_OPENAT, 0, None, &buf[..len])
        .map_err(|_| VfsClientError::MessageFailed)
}

/// Build a `VFS_OP_READ` [`Message`] requesting `len` bytes from `fd`.
///
/// `buf_ptr` is set to 0: the bytes come back inline in the completion, at most
/// `VFS_INLINE_READ_MAX` per request.
pub fn build_read_message(fd: u64, len: usize) -> Result<Message, VfsClientError> {
    let encoded = ReadWriteArgs::new(fd, 0, len as u64).encode();
    Message::with_header(0, VFS_OP_READ, 0, None, &encoded)
        .map_err(|_| VfsClientError::MessageFailed)
}

/// Build a `VFS_OP_CLOSE` [`Message`] for `fd`.
pub fn build_close_message(fd: u64) -> Result<Message, VfsClientError> {
    let encoded = VfsV1Args::new(fd, 0, 0, 0).encode();
    Message::with_header(0, VFS_OP_CLOSE, 0, None, &encoded)
        .map_err(|_| VfsClientError::MessageFailed)
}

/// Build a `VFS_OP_READ_BULK` [`Message`] (legacy transfer-buffer bulk-read codec).
///
/// This codec is retained for its unit tests and ABI completeness, but the bulk-read path it
/// drove was serviced by kernel syscall NR 27 (InitramfsReadChunk), which was removed in Stage
/// 197A. `initramfs_srv` now declines `VFS_OP_READ_BULK`; PM loads every late-service ELF via the
/// mandatory MemoryObject zero-copy grant (`VFS_OP_FILE_GRANT_RO` + `SpawnFromMemoryObject`).
pub fn build_bulk_read_message(
    fd: u64,
    requested_len: u64,
    offset: u64,
    dst_ptr: usize,
) -> Result<Message, VfsClientError> {
    let encoded = BulkReadArgs::new_with_dst(fd, requested_len, offset, dst_ptr as u64).encode();
    Message::with_header(0, VFS_OP_READ_BULK, 0, None, &encoded)
        .map_err(|_| VfsClientError::MessageFailed)
}

// ── Transaction (transport-generic) ───────────────────────────────────────────

/// One request/reply exchange with the VFS: `call` is NR 6, `recv` the blocking receive on the
/// caller's reply endpoint.
pub trait VfsTransport {
    fn call(&mut self, msg: &Message) -> Result<(), SyscallError>;
    fn recv(&mut self) -> Result<Option<ReceivedMessage>, SyscallError>;
}

/// NR 6, then — only when NR 6 succeeded — one receive. A send failure returns at once.
fn transact<T: VfsTransport>(t: &mut T, msg: &Message) -> Result<ReceivedMessage, VfsClientError> {
    t.call(msg).map_err(VfsClientError::Send)?;
    match t.recv() {
        Ok(Some(received)) => Ok(received),
        Ok(None) => Err(VfsClientError::NoReply),
        Err(err) => Err(VfsClientError::Recv(err)),
    }
}

/// Validate `received` as the completion of `expect`. A reply capability never belongs on a
/// completion; a transferred capability only on a successful FILE_GRANT_RO.
fn settle(
    expect: VfsCompletionExpect,
    received: &ReceivedMessage,
) -> Result<VfsCompletionOutcome<'_>, VfsClientError> {
    if received.reply_cap.is_some() {
        return Err(VfsClientError::Malformed(VfsCompletionFault::UnexpectedCap));
    }
    match validate_vfs_completion(
        expect,
        received.message.as_slice(),
        received.transferred_cap.is_some(),
    ) {
        Ok(VfsCompletionOutcome::Status(status)) => Err(VfsClientError::Status(status)),
        Ok(outcome) => Ok(outcome),
        Err(fault) => Err(VfsClientError::Malformed(fault)),
    }
}

fn settled_value(
    expect: VfsCompletionExpect,
    received: &ReceivedMessage,
) -> Result<u64, VfsClientError> {
    match settle(expect, received)? {
        VfsCompletionOutcome::Ok { value, .. } => Ok(value),
        VfsCompletionOutcome::Status(status) => Err(VfsClientError::Status(status)),
    }
}

const fn expect(op: u16, fd: u64, requested: u64) -> VfsCompletionExpect {
    VfsCompletionExpect { op, fd, requested }
}

/// STATX `path`; returns the completion's value.
pub fn vfs_statx_with<T: VfsTransport>(t: &mut T, path: &[u8]) -> Result<u64, VfsClientError> {
    let msg = build_statx_message(path)?;
    let received = transact(t, &msg)?;
    settled_value(expect(VFS_OP_STATX, 0, 0), &received)
}

/// OPENAT `path`; returns the fd.
pub fn vfs_openat_with<T: VfsTransport>(
    t: &mut T,
    path: &[u8],
    flags: u64,
) -> Result<u64, VfsClientError> {
    let msg = build_openat_message(path, flags)?;
    let received = transact(t, &msg)?;
    settled_value(expect(VFS_OP_OPENAT, 0, 0), &received)
}

/// READ up to `min(buf.len(), VFS_INLINE_READ_MAX)` bytes from `fd`.
///
/// Returns the validated count `n` after copying exactly `n` bytes into `buf[..n]`; `buf[n..]` is
/// untouched. `Ok(0)` for a nonempty request is end of file. A short count is a valid read. On any
/// error `buf` is untouched. The request is never retried: a second READ after an ambiguous
/// completion could advance the file cursor twice.
pub fn vfs_read_with<T: VfsTransport>(
    t: &mut T,
    fd: u64,
    buf: &mut [u8],
) -> Result<usize, VfsClientError> {
    let requested = core::cmp::min(buf.len(), VFS_INLINE_READ_MAX);
    let msg = build_read_message(fd, requested)?;
    let received = transact(t, &msg)?;
    match settle(expect(VFS_OP_READ, fd, requested as u64), &received)? {
        VfsCompletionOutcome::Ok { data, .. } => {
            buf[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
        VfsCompletionOutcome::Status(status) => Err(VfsClientError::Status(status)),
    }
}

/// CLOSE `fd`; returns the completion's value (0).
pub fn vfs_close_with<T: VfsTransport>(t: &mut T, fd: u64) -> Result<u64, VfsClientError> {
    let msg = build_close_message(fd)?;
    let received = transact(t, &msg)?;
    settled_value(expect(VFS_OP_CLOSE, fd, 0), &received)
}

/// VFS-LIFETIME1 — FILE_GRANT_RO `fd`: the file length and the read-only MemoryObject capability
/// now in the caller's CNode. The caller owns that capability and releases it (NR 22) when done.
/// A completion this function rejects may still have installed one — the kernel materializes a
/// transferred capability before the reply is validated — so on every error that carried one,
/// `release` is called with it and nothing is returned that the caller could forget.
pub fn vfs_file_grant_ro_with<T: VfsTransport>(
    t: &mut T,
    fd: u64,
    mut release: impl FnMut(u64),
) -> Result<(u64, u64), VfsClientError> {
    let msg = Message::with_header(
        0,
        VFS_OP_FILE_GRANT_RO,
        0,
        None,
        &FileGrantRoArgs::new(fd).encode(),
    )
    .map_err(|_| VfsClientError::MessageFailed)?;
    let received = transact(t, &msg)?;
    let cap = received.transferred_cap;
    match settled_value(expect(VFS_OP_FILE_GRANT_RO, fd, 0), &received) {
        Ok(len) => match cap {
            Some(cap) => Ok((len, cap)),
            None => Err(VfsClientError::Malformed(VfsCompletionFault::MissingCap)),
        },
        Err(e) => {
            if let Some(cap) = cap {
                release(cap);
            }
            Err(e)
        }
    }
}

// ── IPC helpers (unsafe — require live kernel capabilities) ───────────────────

/// Send a `VFS_OP_STATX` request for `path` to `vfs_send_cap` and return
/// the completion's value.
///
/// Blocks until the VFS server replies (uses `ipc_recv_v2`).
///
/// # Safety
/// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a
/// valid RECV capability, both belonging to the calling task's cnode.
pub unsafe fn vfs_statx(
    vfs_send_cap: u64,
    reply_recv_cap: u64,
    path: &[u8],
) -> Result<u64, VfsClientError> {
    // SAFETY: Caller guarantees both caps are valid for this task.
    let mut t = unsafe { SyscallTransport::new(vfs_send_cap, reply_recv_cap) };
    vfs_statx_with(&mut t, path)
}

/// Send a `VFS_OP_OPENAT` request for `path` to `vfs_send_cap` and return
/// the opened file descriptor from the completion.
///
/// Blocks until the VFS server replies (uses `ipc_recv_v2`).
///
/// # Safety
/// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a
/// valid RECV capability, both belonging to the calling task's cnode.
pub unsafe fn vfs_openat(
    vfs_send_cap: u64,
    reply_recv_cap: u64,
    path: &[u8],
    flags: u64,
) -> Result<u64, VfsClientError> {
    // SAFETY: Caller guarantees both caps are valid for this task.
    let mut t = unsafe { SyscallTransport::new(vfs_send_cap, reply_recv_cap) };
    vfs_openat_with(&mut t, path, flags)
}

/// Read from `fd` through `vfs_send_cap`: see [`vfs_read_with`]. Blocks until the VFS server
/// replies (uses `ipc_recv_v2`).
///
/// # Safety
/// Same as [`vfs_statx`].
pub unsafe fn vfs_read(
    vfs_send_cap: u64,
    reply_recv_cap: u64,
    fd: u64,
    buf: &mut [u8],
) -> Result<usize, VfsClientError> {
    // SAFETY: Caller guarantees both caps are valid for this task.
    let mut t = unsafe { SyscallTransport::new(vfs_send_cap, reply_recv_cap) };
    vfs_read_with(&mut t, fd, buf)
}

/// Send a `VFS_OP_CLOSE` request for `fd` to `vfs_send_cap` and return the
/// completion's value (0 on success).  Blocks until the VFS server replies (uses `ipc_recv_v2`).
///
/// # Safety
/// Same as [`vfs_statx`].
pub unsafe fn vfs_close(
    vfs_send_cap: u64,
    reply_recv_cap: u64,
    fd: u64,
) -> Result<u64, VfsClientError> {
    // SAFETY: Caller guarantees both caps are valid for this task.
    let mut t = unsafe { SyscallTransport::new(vfs_send_cap, reply_recv_cap) };
    vfs_close_with(&mut t, fd)
}

/// [`vfs_file_grant_ro_with`] over the production transport; a rejected completion's capability
/// is released through NR 22.
///
/// # Safety
/// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a valid RECV capability,
/// both belonging to the calling task's cnode.
pub unsafe fn vfs_file_grant_ro(
    vfs_send_cap: u64,
    reply_recv_cap: u64,
    fd: u64,
) -> Result<(u64, u64), VfsClientError> {
    // SAFETY: Caller guarantees both caps are valid for this task.
    let mut t = unsafe { SyscallTransport::new(vfs_send_cap, reply_recv_cap) };
    vfs_file_grant_ro_with(&mut t, fd, |cap| {
        let _ = crate::grant_release::relinquish_now(cap);
    })
}

/// The production transport: NR 6 on `vfs_send_cap`, blocking `ipc_recv_v2` on `reply_recv_cap`.
pub struct SyscallTransport {
    vfs_send_cap: u64,
    reply_recv_cap: u64,
}

impl SyscallTransport {
    /// # Safety
    /// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a valid RECV
    /// capability, both belonging to the calling task's cnode.
    pub const unsafe fn new(vfs_send_cap: u64, reply_recv_cap: u64) -> Self {
        Self {
            vfs_send_cap,
            reply_recv_cap,
        }
    }
}

impl VfsTransport for SyscallTransport {
    fn call(&mut self, msg: &Message) -> Result<(), SyscallError> {
        // SAFETY: `new`'s caller guaranteed both caps.
        unsafe { crate::syscall::ipc_call(self.vfs_send_cap, self.reply_recv_cap, msg) }
    }

    fn recv(&mut self) -> Result<Option<ReceivedMessage>, SyscallError> {
        let reply_recv_cap = self.reply_recv_cap;
        // SAFETY: `new`'s caller guaranteed the receive capability.
        unsafe { crate::syscall::ipc_recv_v2(reply_recv_cap) }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::vfs_abi::{OpenAtInlinePath, StatxInlinePath, VFS_OP_OPENAT, VFS_OP_STATX};

    // ── build_statx_message ──────────────────────────────────────────────────

    #[test]
    fn build_statx_sets_opcode_and_encodes_path() {
        let msg = build_statx_message(b"/initramfs/boot-marker").expect("build");
        assert_eq!(msg.opcode, VFS_OP_STATX);
        let decoded = StatxInlinePath::decode(msg.as_slice()).expect("decode");
        assert_eq!(decoded.path, b"/initramfs/boot-marker");
        assert_eq!(decoded.dirfd, 0);
        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.mask_or_buf, 0);
    }

    #[test]
    fn build_statx_encodes_dev_null_path() {
        let msg = build_statx_message(b"/dev/null").expect("build");
        assert_eq!(msg.opcode, VFS_OP_STATX);
        let decoded = StatxInlinePath::decode(msg.as_slice()).expect("decode");
        assert_eq!(decoded.path, b"/dev/null");
    }

    #[test]
    fn build_statx_rejects_empty_path() {
        assert_eq!(build_statx_message(b""), Err(VfsClientError::PathTooLong));
    }

    #[test]
    fn build_statx_rejects_path_over_96_bytes() {
        let long = [b'a'; 97];
        assert_eq!(build_statx_message(&long), Err(VfsClientError::PathTooLong));
    }

    #[test]
    fn build_statx_accepts_exactly_96_byte_path() {
        let max = [b'a'; 96];
        assert!(build_statx_message(&max).is_ok());
    }

    #[test]
    fn build_statx_payload_matches_abi_codec_golden() {
        let path = b"/dev/console";
        let msg = build_statx_message(path).expect("build");
        let (expected_buf, expected_len) = StatxInlinePath {
            dirfd: 0,
            flags: 0,
            mask_or_buf: 0,
            path,
        }
        .encode()
        .expect("direct encode");
        assert_eq!(msg.as_slice(), &expected_buf[..expected_len]);
    }

    // ── build_openat_message ─────────────────────────────────────────────────

    #[test]
    fn build_openat_sets_opcode_path_and_flags() {
        let msg = build_openat_message(b"/initramfs/boot-marker", 0x42).expect("build");
        assert_eq!(msg.opcode, VFS_OP_OPENAT);
        let decoded = OpenAtInlinePath::decode(msg.as_slice()).expect("decode");
        assert_eq!(decoded.path, b"/initramfs/boot-marker");
        assert_eq!(decoded.flags, 0x42);
        assert_eq!(decoded.dirfd, 0);
        assert_eq!(decoded.mode, 0);
    }

    #[test]
    fn build_openat_rejects_empty_path() {
        assert_eq!(
            build_openat_message(b"", 0),
            Err(VfsClientError::PathTooLong)
        );
    }

    #[test]
    fn build_openat_rejects_path_over_96_bytes() {
        let long = [b'a'; 97];
        assert_eq!(
            build_openat_message(&long, 0),
            Err(VfsClientError::PathTooLong)
        );
    }

    #[test]
    fn build_openat_accepts_exactly_96_byte_path() {
        let max = [b'a'; 96];
        assert!(build_openat_message(&max, 0).is_ok());
    }

    #[test]
    fn build_openat_payload_matches_abi_codec_golden() {
        let path = b"/dev/null";
        let msg = build_openat_message(path, 0).expect("build");
        let (expected_buf, expected_len) = OpenAtInlinePath {
            dirfd: 0,
            flags: 0,
            mode: 0,
            path,
        }
        .encode()
        .expect("direct encode");
        assert_eq!(msg.as_slice(), &expected_buf[..expected_len]);
    }

    // ── build_read_message ───────────────────────────────────────────────────

    #[test]
    fn build_read_sets_opcode_fd_and_len() {
        use yarm_ipc_abi::vfs_abi::{ReadWriteArgs, VFS_OP_READ};
        let msg = build_read_message(7, 128).expect("build");
        assert_eq!(msg.opcode, VFS_OP_READ);
        let args = ReadWriteArgs::decode(msg.as_slice()).expect("decode");
        assert_eq!(args.fd, 7);
        assert_eq!(args.len, 128);
        assert_eq!(args.buf_ptr, 0);
    }

    #[test]
    fn build_read_len_zero_is_valid() {
        let msg = build_read_message(3, 0).expect("build");
        use yarm_ipc_abi::vfs_abi::ReadWriteArgs;
        let args = ReadWriteArgs::decode(msg.as_slice()).expect("decode");
        assert_eq!(args.len, 0);
    }

    #[test]
    fn build_read_payload_matches_readwrite_args_golden() {
        use yarm_ipc_abi::vfs_abi::ReadWriteArgs;
        let msg = build_read_message(4, 64).expect("build");
        let expected = ReadWriteArgs::new(4, 0, 64).encode();
        assert_eq!(msg.as_slice(), &expected);
    }

    // ── build_close_message ──────────────────────────────────────────────────

    #[test]
    fn build_close_sets_opcode_and_fd() {
        use yarm_ipc_abi::vfs_abi::{VFS_OP_CLOSE, VfsV1Args};
        let msg = build_close_message(9).expect("build");
        assert_eq!(msg.opcode, VFS_OP_CLOSE);
        let args = VfsV1Args::decode(msg.as_slice()).expect("decode");
        assert_eq!(args.arg0, 9); // fd
        assert_eq!(args.arg1, 0);
        assert_eq!(args.arg2, 0);
        assert_eq!(args.arg3, 0);
    }

    #[test]
    fn build_close_fd_zero_encodes() {
        use yarm_ipc_abi::vfs_abi::{VFS_OP_CLOSE, VfsV1Args};
        let msg = build_close_message(0).expect("build");
        assert_eq!(msg.opcode, VFS_OP_CLOSE);
        let args = VfsV1Args::decode(msg.as_slice()).expect("decode");
        assert_eq!(args.arg0, 0);
    }

    #[test]
    fn build_close_payload_matches_vfs_v1_args_golden() {
        use yarm_ipc_abi::vfs_abi::VfsV1Args;
        let msg = build_close_message(42).expect("build");
        let expected = VfsV1Args::new(42, 0, 0, 0).encode();
        assert_eq!(msg.as_slice(), &expected);
    }

    // ── Cross-operation checks ───────────────────────────────────────────────

    #[test]
    fn statx_and_openat_opcodes_are_distinct() {
        let statx = build_statx_message(b"/x").expect("statx");
        let openat = build_openat_message(b"/x", 0).expect("openat");
        assert_ne!(statx.opcode, openat.opcode);
        assert_eq!(statx.opcode, VFS_OP_STATX);
        assert_eq!(openat.opcode, VFS_OP_OPENAT);
    }

    #[test]
    fn all_three_opcodes_are_distinct() {
        use yarm_ipc_abi::vfs_abi::{VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX};
        let statx = build_statx_message(b"/x").expect("statx");
        let openat = build_openat_message(b"/x", 0).expect("openat");
        let read = build_read_message(3, 64).expect("read");
        assert_eq!(statx.opcode, VFS_OP_STATX);
        assert_eq!(openat.opcode, VFS_OP_OPENAT);
        assert_eq!(read.opcode, VFS_OP_READ);
        assert_ne!(statx.opcode, openat.opcode);
        assert_ne!(statx.opcode, read.opcode);
        assert_ne!(openat.opcode, read.opcode);
    }

    #[test]
    fn all_four_opcodes_are_distinct() {
        use yarm_ipc_abi::vfs_abi::{VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_STATX};
        let statx = build_statx_message(b"/x").expect("statx");
        let openat = build_openat_message(b"/x", 0).expect("openat");
        let read = build_read_message(3, 64).expect("read");
        let close = build_close_message(3).expect("close");
        assert_eq!(statx.opcode, VFS_OP_STATX);
        assert_eq!(openat.opcode, VFS_OP_OPENAT);
        assert_eq!(read.opcode, VFS_OP_READ);
        assert_eq!(close.opcode, VFS_OP_CLOSE);
        assert_ne!(close.opcode, statx.opcode);
        assert_ne!(close.opcode, openat.opcode);
        assert_ne!(close.opcode, read.opcode);
    }

    // ── VFS-CORRECT1A: the transaction through a scripted transport ─────────────

    use yarm_ipc_abi::vfs_abi::{
        VFS_OP_FILE_GRANT_RO, VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_INVALID_PATH, VfsCompletion,
    };

    const SENTINEL: u8 = 0xA5;

    /// One scripted reply: payload, transferred cap, reply cap.
    #[derive(Clone, Copy)]
    struct Scripted {
        payload: [u8; 128],
        len: usize,
        transferred_cap: Option<u64>,
        reply_cap: Option<u64>,
    }

    impl Scripted {
        fn completion(c: VfsCompletion, data: &[u8]) -> Self {
            let (payload, len) = c.encode(data).expect("encode");
            Self {
                payload,
                len,
                transferred_cap: None,
                reply_cap: None,
            }
        }
        fn raw(bytes: &[u8]) -> Self {
            let mut payload = [0u8; 128];
            payload[..bytes.len()].copy_from_slice(bytes);
            Self {
                payload,
                len: bytes.len(),
                transferred_cap: None,
                reply_cap: None,
            }
        }
    }

    /// Records every call and receive; replies from a fixed script.
    struct ScriptTransport {
        send_result: Result<(), SyscallError>,
        replies: [Option<Scripted>; 4],
        next: usize,
        calls: usize,
        recvs: usize,
        last_request: Option<Message>,
    }

    impl ScriptTransport {
        fn new(send_result: Result<(), SyscallError>) -> Self {
            Self {
                send_result,
                replies: [None; 4],
                next: 0,
                calls: 0,
                recvs: 0,
                last_request: None,
            }
        }
        fn reply(mut self, r: Scripted) -> Self {
            let i = self.replies.iter().position(|x| x.is_none()).expect("room");
            self.replies[i] = Some(r);
            self
        }
    }

    impl VfsTransport for ScriptTransport {
        fn call(&mut self, msg: &Message) -> Result<(), SyscallError> {
            self.calls += 1;
            self.last_request = Some(*msg);
            self.send_result
        }
        fn recv(&mut self) -> Result<Option<ReceivedMessage>, SyscallError> {
            self.recvs += 1;
            let Some(r) = self.replies.get(self.next).copied().flatten() else {
                return Ok(None);
            };
            self.next += 1;
            let flags = if r.transferred_cap.is_some() {
                Message::FLAG_CAP_TRANSFER
            } else {
                0
            };
            let message = Message::with_header(
                10000,
                0,
                flags,
                r.transferred_cap.map(u64::from),
                &r.payload[..r.len],
            )
            .expect("message");
            Ok(Some(ReceivedMessage {
                message,
                reply_cap: r.reply_cap,
                transferred_cap: r.transferred_cap,
                sender_tid: 10000,
            }))
        }
    }

    fn read_ok(fd: u64, data: &[u8]) -> Scripted {
        Scripted::completion(VfsCompletion::ok(VFS_OP_READ, data.len() as u64, fd), data)
    }

    #[test]
    fn vc1a_send_failure_makes_zero_receive_attempts() {
        for err in [
            SyscallError::WouldBlock,
            SyscallError::InvalidCapability,
            SyscallError::QueueFull,
        ] {
            let mut t = ScriptTransport::new(Err(err)).reply(read_ok(3, b"never"));
            let mut buf = [SENTINEL; 16];
            assert_eq!(
                vfs_read_with(&mut t, 3, &mut buf),
                Err(VfsClientError::Send(err))
            );
            assert_eq!((t.calls, t.recvs), (1, 0));
            assert!(buf.iter().all(|&b| b == SENTINEL));
            let mut t = ScriptTransport::new(Err(err));
            assert_eq!(
                vfs_openat_with(&mut t, b"/initramfs/x", 0),
                Err(VfsClientError::Send(err))
            );
            assert_eq!((t.calls, t.recvs), (1, 0));
            let mut t = ScriptTransport::new(Err(err));
            assert_eq!(
                vfs_statx_with(&mut t, b"/x"),
                Err(VfsClientError::Send(err))
            );
            assert_eq!((t.calls, t.recvs), (1, 0));
            let mut t = ScriptTransport::new(Err(err));
            assert_eq!(vfs_close_with(&mut t, 3), Err(VfsClientError::Send(err)));
            assert_eq!((t.calls, t.recvs), (1, 0));
        }
    }

    #[test]
    fn vc1a_read_copies_exactly_the_count_and_leaves_the_tail() {
        let mut t = ScriptTransport::new(Ok(())).reply(read_ok(7, b"hello"));
        let mut buf = [SENTINEL; 32];
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(5));
        assert_eq!(&buf[..5], b"hello");
        assert!(buf[5..].iter().all(|&b| b == SENTINEL));
        assert_eq!((t.calls, t.recvs), (1, 1));
        // The request asked for the slice length (32 < inline max).
        let req = t.last_request.expect("request");
        assert_eq!(ReadWriteArgs::decode(req.as_slice()).expect("args").len, 32);
    }

    #[test]
    fn vc1a_large_destination_asks_for_one_inline_reply_and_accepts_a_short_read() {
        let data = [0x3Cu8; VFS_INLINE_READ_MAX];
        let mut t = ScriptTransport::new(Ok(())).reply(read_ok(7, &data));
        let mut buf = [SENTINEL; 512];
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(VFS_INLINE_READ_MAX));
        assert!(buf[..VFS_INLINE_READ_MAX].iter().all(|&b| b == 0x3C));
        assert!(buf[VFS_INLINE_READ_MAX..].iter().all(|&b| b == SENTINEL));
        let req = t.last_request.expect("request");
        assert_eq!(
            ReadWriteArgs::decode(req.as_slice()).expect("args").len,
            VFS_INLINE_READ_MAX as u64
        );
    }

    #[test]
    fn vc1a_eof_repeats_and_zero_length_reads_are_valid() {
        let mut t = ScriptTransport::new(Ok(()))
            .reply(read_ok(7, &[]))
            .reply(read_ok(7, &[]));
        let mut buf = [SENTINEL; 8];
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(0));
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(0));
        assert!(buf.iter().all(|&b| b == SENTINEL));
        let mut t = ScriptTransport::new(Ok(())).reply(read_ok(7, &[]));
        let mut empty: [u8; 0] = [];
        assert_eq!(vfs_read_with(&mut t, 7, &mut empty), Ok(0));
        let req = t.last_request.expect("request");
        assert_eq!(ReadWriteArgs::decode(req.as_slice()).expect("args").len, 0);
    }

    #[test]
    fn vc1a_malformed_completions_leave_the_destination_untouched() {
        let mut over = VfsCompletion::ok(VFS_OP_READ, 3, 7);
        let cases: [(Scripted, VfsCompletionFault); 7] = [
            // Truncated: the legacy 8-byte ReadLen reply.
            (
                Scripted::raw(&5u64.to_le_bytes()),
                VfsCompletionFault::Truncated,
            ),
            // Count without bytes (the baseline defect's shape).
            (
                Scripted::completion(VfsCompletion::ok(VFS_OP_READ, 4, 7), &[]),
                VfsCompletionFault::CountMismatch {
                    count: 4,
                    data_len: 0,
                },
            ),
            // More bytes than the count.
            (
                {
                    over.value = 2;
                    Scripted::completion(over, b"abc")
                },
                VfsCompletionFault::CountMismatch {
                    count: 2,
                    data_len: 3,
                },
            ),
            // More than requested.
            (
                read_ok(7, &[1u8; 9]),
                VfsCompletionFault::CountExceedsRequest {
                    count: 9,
                    requested: 8,
                },
            ),
            // Another operation's completion.
            (
                Scripted::completion(VfsCompletion::ok(VFS_OP_CLOSE, 0, 7), &[]),
                VfsCompletionFault::WrongOp {
                    expected: VFS_OP_READ,
                    got: VFS_OP_CLOSE,
                },
            ),
            // Another fd's completion.
            (
                read_ok(8, b"z"),
                VfsCompletionFault::WrongFd {
                    expected: 7,
                    got: 8,
                },
            ),
            // A capability on a READ completion.
            (
                {
                    let mut r = read_ok(7, b"z");
                    r.transferred_cap = Some(77);
                    r
                },
                VfsCompletionFault::UnexpectedCap,
            ),
        ];
        for (reply, fault) in cases {
            let mut t = ScriptTransport::new(Ok(())).reply(reply);
            let mut buf = [SENTINEL; 8];
            assert_eq!(
                vfs_read_with(&mut t, 7, &mut buf),
                Err(VfsClientError::Malformed(fault))
            );
            assert!(buf.iter().all(|&b| b == SENTINEL), "{:?}", fault);
            // No retry: one call, one receive.
            assert_eq!((t.calls, t.recvs), (1, 1));
        }
        // A reply capability never belongs on a completion.
        let mut r = read_ok(7, b"z");
        r.reply_cap = Some(5);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        let mut buf = [SENTINEL; 8];
        assert_eq!(
            vfs_read_with(&mut t, 7, &mut buf),
            Err(VfsClientError::Malformed(VfsCompletionFault::UnexpectedCap))
        );
        assert!(buf.iter().all(|&b| b == SENTINEL));
    }

    #[test]
    fn vc1a_status_errors_are_distinct_from_transport_and_malformed() {
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::err(VFS_OP_READ, VFS_STATUS_ERR_BAD_FD, 99),
            &[],
        ));
        let mut buf = [SENTINEL; 8];
        assert_eq!(
            vfs_read_with(&mut t, 99, &mut buf),
            Err(VfsClientError::Status(VFS_STATUS_ERR_BAD_FD))
        );
        assert!(buf.iter().all(|&b| b == SENTINEL));
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::err(VFS_OP_OPENAT, VFS_STATUS_ERR_INVALID_PATH, 0),
            &[],
        ));
        assert_eq!(
            vfs_openat_with(&mut t, b"/initramfs/missing", 0),
            Err(VfsClientError::Status(VFS_STATUS_ERR_INVALID_PATH))
        );
        // Nothing received at all.
        let mut t = ScriptTransport::new(Ok(()));
        assert_eq!(vfs_close_with(&mut t, 3), Err(VfsClientError::NoReply));
        assert_eq!((t.calls, t.recvs), (1, 1));
    }

    #[test]
    fn vc1a_openat_statx_close_return_validated_values() {
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::ok(VFS_OP_OPENAT, 14, 0),
            &[],
        ));
        assert_eq!(vfs_openat_with(&mut t, b"/initramfs/x", 0), Ok(14));
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::ok(VFS_OP_STATX, 300, 0),
            &[],
        ));
        assert_eq!(vfs_statx_with(&mut t, b"/initramfs/x"), Ok(300));
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::ok(VFS_OP_CLOSE, 0, 14),
            &[],
        ));
        assert_eq!(vfs_close_with(&mut t, 14), Ok(0));
        // An OPENAT completion carrying a capability is refused before the fd is returned.
        let mut r = Scripted::completion(VfsCompletion::ok(VFS_OP_OPENAT, 14, 0), &[]);
        r.transferred_cap = Some(9);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        assert_eq!(
            vfs_openat_with(&mut t, b"/initramfs/x", 0),
            Err(VfsClientError::Malformed(VfsCompletionFault::UnexpectedCap))
        );
        // A grant completion answering a CLOSE is another operation's reply.
        let mut r = Scripted::completion(VfsCompletion::ok(VFS_OP_FILE_GRANT_RO, 10, 14), &[]);
        r.transferred_cap = Some(9);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        assert_eq!(
            vfs_close_with(&mut t, 14),
            Err(VfsClientError::Malformed(VfsCompletionFault::WrongOp {
                expected: VFS_OP_CLOSE,
                got: VFS_OP_FILE_GRANT_RO
            }))
        );
    }

    // ── VFS-LIFETIME1: the grant client owns, or releases, what it received ───────

    #[test]
    fn vl1_grant_client_returns_the_capability_or_releases_a_rejected_one() {
        let mut r = Scripted::completion(VfsCompletion::ok(VFS_OP_FILE_GRANT_RO, 300, 14), &[]);
        r.transferred_cap = Some(9);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        let mut released = None;
        assert_eq!(
            vfs_file_grant_ro_with(&mut t, 14, |c| released = Some(c)),
            Ok((300, 9))
        );
        assert_eq!(
            released, None,
            "a returned capability is the caller's, not released"
        );
        // The wrong fd echoed, with a capability installed: rejected AND released.
        let mut r = Scripted::completion(VfsCompletion::ok(VFS_OP_FILE_GRANT_RO, 300, 15), &[]);
        r.transferred_cap = Some(11);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        assert_eq!(
            vfs_file_grant_ro_with(&mut t, 14, |c| released = Some(c)),
            Err(VfsClientError::Malformed(VfsCompletionFault::WrongFd {
                expected: 14,
                got: 15
            }))
        );
        assert_eq!(released, Some(11));
        released = None;
        // An error completion with no capability releases nothing.
        let r = Scripted::completion(
            VfsCompletion::err(VFS_OP_FILE_GRANT_RO, VFS_STATUS_ERR_BAD_FD, 14),
            &[],
        );
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        assert_eq!(
            vfs_file_grant_ro_with(&mut t, 14, |c| released = Some(c)),
            Err(VfsClientError::Status(VFS_STATUS_ERR_BAD_FD))
        );
        assert_eq!(released, None);
    }

    // ── Phase 2B build_bulk_read_message tests ────────────────────────────────

    /// Phase 2B: build_bulk_read_message encodes fd, requested_len, offset, and dst_ptr.
    #[test]
    fn build_bulk_read_message_encodes_dst_ptr() {
        use yarm_ipc_abi::vfs_abi::{BulkReadArgs, VFS_OP_READ_BULK};
        let dst_ptr = 0xDEAD_BEEF_0000usize;
        let msg = build_bulk_read_message(7, 4096, 8192, dst_ptr).expect("bulk msg");
        assert_eq!(msg.opcode, VFS_OP_READ_BULK);
        let decoded = BulkReadArgs::decode(msg.as_slice()).expect("decode bulk args");
        assert_eq!(decoded.fd, 7);
        assert_eq!(decoded.requested_len, 4096);
        assert_eq!(decoded.offset, 8192);
        assert_eq!(decoded.dst_ptr, dst_ptr as u64);
    }

    /// Phase 2B: build_bulk_read_message with dst_ptr=0 encodes as zero (Phase 2A compat).
    #[test]
    fn build_bulk_read_message_zero_dst_ptr_is_phase2a_compat() {
        use yarm_ipc_abi::vfs_abi::{BulkReadArgs, VFS_OP_READ_BULK};
        let msg = build_bulk_read_message(3, 512, 0, 0).expect("bulk msg");
        assert_eq!(msg.opcode, VFS_OP_READ_BULK);
        let decoded = BulkReadArgs::decode(msg.as_slice()).expect("decode");
        assert_eq!(decoded.dst_ptr, 0);
    }
}
