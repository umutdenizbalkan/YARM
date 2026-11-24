cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-wt/crates/yarm-user-rt/src; python3 - <<'EOF'
p="vfs_client.rs"
s=open(p).read()
start=s.index("//! Reusable VFS client helpers")
end=s.index("// ── Tests ─────")
new_head='''//! Reusable VFS client helpers for userspace tasks.
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
    BulkReadArgs, OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_INLINE_READ_MAX,
    VFS_OP_CLOSE, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_READ_BULK, VFS_OP_STATX,
    VfsCompletionExpect, VfsCompletionFault, VfsCompletionOutcome, VfsV1Args,
    validate_vfs_completion,
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
    vfs_send_cap: u32,
    reply_recv_cap: u32,
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
    vfs_send_cap: u32,
    reply_recv_cap: u32,
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
    vfs_send_cap: u32,
    reply_recv_cap: u32,
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
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    fd: u64,
) -> Result<u64, VfsClientError> {
    // SAFETY: Caller guarantees both caps are valid for this task.
    let mut t = unsafe { SyscallTransport::new(vfs_send_cap, reply_recv_cap) };
    vfs_close_with(&mut t, fd)
}

/// The production transport: NR 6 on `vfs_send_cap`, blocking `ipc_recv_v2` on `reply_recv_cap`.
pub struct SyscallTransport {
    vfs_send_cap: u32,
    reply_recv_cap: u32,
}

impl SyscallTransport {
    /// # Safety
    /// `vfs_send_cap` must be a valid SEND capability and `reply_recv_cap` a valid RECV
    /// capability, both belonging to the calling task's cnode.
    pub const unsafe fn new(vfs_send_cap: u32, reply_recv_cap: u32) -> Self {
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

'''
s=s[:start]+new_head+s[end:]
open(p,"w").write(s)
EOF
cd ../../.. && env -u CARGO_TARGET_DIR cargo check -q -p yarm-user-rt 2>&1 | tail -20