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
//! `vfs_openat`) are `unsafe` because they invoke the kernel via
//! `ipc_call` + `ipc_recv_with_deadline`.

use crate::ipc::Message;
use yarm_ipc_abi::vfs_abi::{
    OpenAtInlinePath, StatxInlinePath, VFS_OP_OPENAT, VFS_OP_STATX,
};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during a VFS client operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsClientError {
    /// Path is empty or exceeds the inline-path maximum (96 bytes).
    PathTooLong,
    /// IPC message construction failed (internal payload overflow).
    MessageFailed,
    /// `ipc_recv_with_deadline` returned no message within the deadline.
    NoReply,
    /// Reply payload is too short to hold a valid `u64` status value.
    Malformed,
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

// ── Internal reply decoder ────────────────────────────────────────────────────

fn decode_reply_u64(reply: &Message) -> Result<u64, VfsClientError> {
    let payload = reply.as_slice();
    if payload.len() < 8 {
        return Err(VfsClientError::Malformed);
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&payload[..8]);
    Ok(u64::from_le_bytes(b))
}

// ── IPC helpers (unsafe — require live kernel capabilities) ───────────────────

/// Send a `VFS_OP_STATX` request for `path` to `vfs_send_cap` and return
/// the decoded reply status.
///
/// Uses a zero-tick deadline; the call never blocks if the server is stalled.
///
/// # Safety
/// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a
/// valid RECV capability, both belonging to the calling task's cnode.
pub unsafe fn vfs_statx(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    path: &[u8],
) -> Result<u64, VfsClientError> {
    let msg = build_statx_message(path)?;
    // SAFETY: Caller guarantees both caps are valid for this task.
    let _ = unsafe { crate::syscall::ipc_call(vfs_send_cap, reply_recv_cap, &msg) };
    match unsafe { crate::syscall::ipc_recv_with_deadline(reply_recv_cap, 0) } {
        Ok(Some(ref r)) => decode_reply_u64(r),
        _ => Err(VfsClientError::NoReply),
    }
}

/// Send a `VFS_OP_OPENAT` request for `path` to `vfs_send_cap` and return
/// the opened file descriptor from the reply.
///
/// Uses a zero-tick deadline; the call never blocks if the server is stalled.
///
/// # Safety
/// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a
/// valid RECV capability, both belonging to the calling task's cnode.
pub unsafe fn vfs_openat(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    path: &[u8],
    flags: u64,
) -> Result<u64, VfsClientError> {
    let msg = build_openat_message(path, flags)?;
    // SAFETY: Caller guarantees both caps are valid for this task.
    let _ = unsafe { crate::syscall::ipc_call(vfs_send_cap, reply_recv_cap, &msg) };
    match unsafe { crate::syscall::ipc_recv_with_deadline(reply_recv_cap, 0) } {
        Ok(Some(ref r)) => decode_reply_u64(r),
        _ => Err(VfsClientError::NoReply),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::vfs_abi::{
        OpenAtInlinePath, StatxInlinePath, VFS_OP_OPENAT, VFS_OP_STATX,
    };

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

    // ── Cross-operation checks ───────────────────────────────────────────────

    #[test]
    fn statx_and_openat_opcodes_are_distinct() {
        let statx = build_statx_message(b"/x").expect("statx");
        let openat = build_openat_message(b"/x", 0).expect("openat");
        assert_ne!(statx.opcode, openat.opcode);
        assert_eq!(statx.opcode, VFS_OP_STATX);
        assert_eq!(openat.opcode, VFS_OP_OPENAT);
    }
}
