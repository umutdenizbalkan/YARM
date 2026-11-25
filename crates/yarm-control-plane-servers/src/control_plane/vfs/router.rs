// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-CORRECT1C — the resident VFS's request router: the client fd namespace, recoverable
//! open/close, session ownership and canonical paths. Canonical contract: `doc/VFS.md` §3.3.
//!
//! Every request is handled in this order:
//!
//! 1. **Who.** The kernel names the caller's session from the request's reply capability
//!    (NR 21 op 0). No session, no service: `PERMISSION`, nothing is forwarded.
//! 2. **Housekeeping.** One undelivered backend close is retried (same instance, same handle);
//!    every other session holding descriptors is asked whether it still has a live thread
//!    (NR 21 op 1), and one that has none is reclaimed: its descriptors go and the backend
//!    handles they alone held are closed.
//! 3. **Route.** `STATX` / `OPENAT`: the path is normalized ONCE; the canonical form selects the
//!    mount (longest prefix) and is the path the backend receives. `READ` / `CLOSE` /
//!    `FILE_GRANT_RO`: the client fd is resolved in the caller's session to its open file
//!    description — the exact backend instance and its handle — and the request is re-encoded
//!    with the backend handle.
//! 4. **Validate, then translate back.** The backend completion is validated against the
//!    operation and the handle actually forwarded; the client completion is then built with the
//!    client fd (READ bytes, status and the grant capability preserved).
//!
//! `OPENAT` reserves a descriptor before the backend is asked, so a full table answers `NO_FD`
//! with nothing opened. If the backend opened the file but the client cannot be told, the open is
//! rolled back: the exact handle is closed at the same instance.

use super::mount_table::{MountLabel, VfsMountTable};
#[cfg(test)]
use yarm_fs_servers::common::fd_space::Counts;
use yarm_fs_servers::common::fd_space::{
    BackendClose, BackendRef, CloseOutcome, FdSpace, ReserveError, ResolveError, SessionId,
};
use yarm_ipc_abi::vfs_abi::{
    FileGrantRoArgs, OpenAtInlinePath, ReadWriteArgs, StatxInlinePath, VFS_INLINE_READ_MAX,
    VFS_OP_CLOSE, VFS_OP_FILE_GRANT_RO, VFS_OP_OPENAT, VFS_OP_READ, VFS_OP_READ_BULK, VFS_OP_STATX,
    VFS_STATUS_ERR_BACKEND, VFS_STATUS_ERR_BACKEND_MALFORMED, VFS_STATUS_ERR_BAD_FD,
    VFS_STATUS_ERR_CODEC, VFS_STATUS_ERR_INVALID_PATH, VFS_STATUS_ERR_NO_FD,
    VFS_STATUS_ERR_NO_MOUNT, VFS_STATUS_ERR_PERMISSION, VFS_STATUS_ERR_UNKNOWN_OP,
    VFS_STATUS_ERR_UNSUPPORTED, VfsCompletion, VfsCompletionExpect, VfsCompletionOutcome,
    validate_vfs_completion,
};
use yarm_user_rt::ipc::Message;

/// What one forwarded backend call came to.
pub(crate) enum CallResult {
    /// The NR 6 call failed: nothing reached the backend.
    NotDelivered,
    /// The call was delivered (or may have been) but no reply was received.
    NoReply,
    Reply {
        message: Message,
        /// A capability that arrived with the reply (transferred, or a reply capability).
        has_cap: bool,
        transferred_cap: Option<u64>,
    },
}

/// The router's view of the kernel. Production: [`super::service`]'s `KernelRouterIo`.
pub(crate) trait RouterIo {
    /// One NR 6 call to `backend` and the receive of its answer on VFS's backend reply endpoint.
    fn call_backend(&mut self, backend: BackendRef, label: &str, request: &Message) -> CallResult;
    /// NR 7 through `reply_cap`; `false` if the kernel refused it (nothing reached the client).
    fn reply_client(&mut self, reply_cap: u64, reply: &Message) -> bool;
    /// NR 21 op 0: the session and TID of the caller blocked on `reply_cap`.
    fn caller_session(&mut self, reply_cap: u64) -> Option<(SessionId, u64)>;
    /// NR 21 op 1: `Some(false)` only when the kernel says the session has no live thread.
    fn session_alive(&mut self, session: SessionId) -> Option<bool>;
}

/// A validated backend answer, owned (the reply message is gone once this is built).
enum Forwarded {
    Ok {
        value: u64,
        data: [u8; VFS_INLINE_READ_MAX],
        len: usize,
        cap: Option<u64>,
    },
    Status(u32),
    Malformed,
    NotDelivered,
    NoReply,
}

/// How a backend close went, and the value of a successful one.
struct Closed {
    outcome: CloseOutcome,
    value: u64,
}

pub(crate) struct Router {
    pub(crate) fds: FdSpace,
    /// OPENATs whose backend state cannot be known (no reply, or a malformed one): the backend
    /// may hold a handle nobody can name. Never closed by guess — the number may be reused.
    pub(crate) unknown_backend_opens: u64,
    /// `vfs-fd-witness` fault injection: an OPENAT of exactly this canonical path is answered
    /// first through a stale reply capability (the same slot, the next generation), which the
    /// kernel refuses — so the client reply really fails and the open is rolled back — and then
    /// through the real one with `BACKEND`.
    pub(crate) fail_reply_path: Option<&'static [u8]>,
}

fn completion(c: VfsCompletion, data: &[u8], cap: Option<u64>) -> Message {
    let (bytes, len) = c.encode(data).expect("completion fits one reply");
    let flags = if cap.is_some() {
        Message::FLAG_CAP_TRANSFER
    } else {
        0
    };
    Message::with_header(0, 0, flags, cap, &bytes[..len]).expect("completion message")
}

fn error(op: u16, status: u32, fd: u64) -> Message {
    completion(VfsCompletion::err(op, status, fd), &[], None)
}

fn request(op: u16, payload: &[u8]) -> Message {
    Message::with_header(0, op, 0, None, payload).expect("request fits one call")
}

fn path_str(p: &[u8]) -> &str {
    core::str::from_utf8(p).unwrap_or("?")
}

impl Router {
    pub(crate) const fn new() -> Self {
        Self {
            fds: FdSpace::new(),
            unknown_backend_opens: 0,
            fail_reply_path: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn counts(&self) -> Counts {
        self.fds.counts()
    }

    fn log_state(&self, why: &str) {
        let c = self.fds.counts();
        yarm_user_rt::user_log!(
            "VFS_FD_STATE why={} descriptors={} ofds={} sessions={} pending_retry={} quarantined={} pending_overflow={} unknown_backend_opens={}",
            why,
            c.descriptors,
            c.ofds,
            c.sessions,
            c.pending_retry,
            c.quarantined,
            c.pending_overflow,
            self.unknown_backend_opens
        );
    }

    /// Handle one client request (every operation except mount administration).
    pub(crate) fn handle<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        msg: &Message,
        reply_cap: u64,
    ) {
        let op = msg.opcode;
        let Some((session, caller_tid)) = io.caller_session(reply_cap) else {
            yarm_user_rt::user_log!(
                "VFS_SESSION_UNKNOWN op={} sender_tid={} status={}",
                op,
                msg.sender_tid.0,
                VFS_STATUS_ERR_PERMISSION
            );
            let _ = io.reply_client(reply_cap, &error(op, VFS_STATUS_ERR_PERMISSION, 0));
            return;
        };
        self.housekeeping(io, mounts, session);
        let reply = match op {
            VFS_OP_OPENAT => return self.openat(io, mounts, session, caller_tid, msg, reply_cap),
            VFS_OP_STATX => self.statx(io, mounts, msg),
            VFS_OP_READ => self.read(io, mounts, session, msg),
            VFS_OP_CLOSE => self.close(io, mounts, session, caller_tid, msg),
            VFS_OP_FILE_GRANT_RO => self.grant(io, mounts, session, msg),
            VFS_OP_READ_BULK => {
                // Retired: its transfer-buffer reply carries no completion VFS could validate and
                // no fd it could translate.
                let fd = ReadWriteArgs::decode(msg.as_slice())
                    .map(|a| a.fd)
                    .unwrap_or(0);
                yarm_user_rt::user_log!("VFS_ROUTE_UNSUPPORTED op={} fd={}", op, fd);
                error(op, VFS_STATUS_ERR_UNSUPPORTED, fd)
            }
            _ => {
                yarm_user_rt::user_log!("VFS_ROUTE_UNKNOWN_OP op={}", op);
                error(op, VFS_STATUS_ERR_UNKNOWN_OP, 0)
            }
        };
        if !io.reply_client(reply_cap, &reply) {
            yarm_user_rt::user_log!(
                "VFS_CLIENT_REPLY_FAIL op={} tgid={} has_cap={}",
                op,
                session.thread_group_id,
                reply.transferred_cap().is_some() as u8
            );
        }
    }

    // ── housekeeping ───────────────────────────────────────────────────────────────────────

    fn housekeeping<I: RouterIo>(&mut self, io: &mut I, mounts: &VfsMountTable, caller: SessionId) {
        if let Some(c) = self.fds.next_retry() {
            let r = self.backend_close(io, mounts, c);
            yarm_user_rt::user_log!(
                "VFS_CLOSE_RETRY instance={} handle={} outcome={:?}",
                c.backend.instance,
                c.handle,
                r.outcome
            );
            self.fds.close_settled(c, r.outcome);
            self.log_state("retry");
        }
        let (sessions, n) = self.fds.sessions();
        for s in sessions[..n].iter().flatten() {
            if *s == caller {
                continue;
            }
            if io.session_alive(*s) == Some(false) {
                self.reclaim(io, mounts, *s);
            }
        }
    }

    /// The session has no live thread: every descriptor it held goes, and every backend handle
    /// only it referred to is closed.
    pub(crate) fn reclaim<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        session: SessionId,
    ) {
        let (closes, removed) = self.fds.reclaim_session(session);
        let mut closed = 0;
        for c in closes.iter().flatten() {
            let r = self.backend_close(io, mounts, *c);
            if r.outcome == CloseOutcome::Closed {
                closed += 1;
            }
            self.fds.close_settled(*c, r.outcome);
        }
        yarm_user_rt::user_log!(
            "VFS_SESSION_RECLAIM tgid={} incarnation={} descriptors={} backend_closed={}",
            session.thread_group_id,
            session.incarnation,
            removed,
            closed
        );
        self.log_state("reclaim");
    }

    /// The backend instance is gone (restart): its descriptors go stale, its pending closes go.
    /// No live path removes or replaces a mount yet (hosted only); without this call an old fd is
    /// still refused, because [`Self::target`] forwards only to the registration it was opened on.
    #[cfg(test)]
    pub(crate) fn instance_gone(&mut self, instance: u64) {
        let n = self.fds.invalidate_instance(instance);
        yarm_user_rt::user_log!("VFS_INSTANCE_GONE instance={} stale_ofds={}", instance, n);
        self.log_state("instance_gone");
    }

    // ── forwarding ─────────────────────────────────────────────────────────────────────────

    fn forward<I: RouterIo>(
        io: &mut I,
        backend: BackendRef,
        label: &str,
        req: &Message,
        expect: VfsCompletionExpect,
    ) -> Forwarded {
        match io.call_backend(backend, label, req) {
            CallResult::NotDelivered => Forwarded::NotDelivered,
            CallResult::NoReply => Forwarded::NoReply,
            CallResult::Reply {
                message,
                has_cap,
                transferred_cap,
            } => match validate_vfs_completion(expect, message.as_slice(), has_cap) {
                Ok(VfsCompletionOutcome::Ok { value, data }) => {
                    let mut buf = [0u8; VFS_INLINE_READ_MAX];
                    buf[..data.len()].copy_from_slice(data);
                    Forwarded::Ok {
                        value,
                        data: buf,
                        len: data.len(),
                        cap: transferred_cap,
                    }
                }
                Ok(VfsCompletionOutcome::Status(s)) => {
                    yarm_user_rt::user_log!(
                        "VFS_BACKEND_STATUS op={} handle={} status={}",
                        expect.op,
                        expect.fd,
                        s
                    );
                    Forwarded::Status(s)
                }
                Err(fault) => {
                    yarm_user_rt::user_log!(
                        "VFS_BACKEND_COMPLETION_MALFORMED op={} handle={} fault={:?} has_cap={}",
                        expect.op,
                        expect.fd,
                        fault,
                        has_cap
                    );
                    Forwarded::Malformed
                }
            },
        }
    }

    /// Close `c.handle` at exactly `c.backend`. The registration must still be the one in the
    /// table (same instance, same capability); otherwise there is nothing open there any more.
    fn backend_close<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        c: BackendClose,
    ) -> Closed {
        let label = match mounts.instance(c.backend.instance) {
            Some((cap, label)) if cap == c.backend.cap => label,
            _ => {
                return Closed {
                    outcome: CloseOutcome::Refused(VFS_STATUS_ERR_BAD_FD),
                    value: 0,
                };
            }
        };
        let req = request(VFS_OP_CLOSE, &ReadWriteArgs::new(c.handle, 0, 0).encode());
        let expect = VfsCompletionExpect {
            op: VFS_OP_CLOSE,
            fd: c.handle,
            requested: 0,
        };
        match Self::forward(io, c.backend, label.as_str(), &req, expect) {
            Forwarded::Ok { value, .. } => Closed {
                outcome: CloseOutcome::Closed,
                value,
            },
            Forwarded::Status(s) => Closed {
                outcome: CloseOutcome::Refused(s),
                value: 0,
            },
            Forwarded::NotDelivered => Closed {
                outcome: CloseOutcome::NotDelivered,
                value: 0,
            },
            Forwarded::Malformed | Forwarded::NoReply => Closed {
                outcome: CloseOutcome::Ambiguous,
                value: 0,
            },
        }
    }

    /// Resolve a client fd to a target that may be forwarded: in this session, open, and its
    /// backend registration still the one in the table.
    fn target(
        &self,
        mounts: &VfsMountTable,
        session: SessionId,
        op: u16,
        fd: u64,
    ) -> Result<(BackendRef, u64, MountLabel), Message> {
        let t = match self.fds.resolve(session, fd) {
            Ok(t) => t,
            Err(e) => {
                yarm_user_rt::user_log!(
                    "VFS_FD_BAD op={} fd={} tgid={} reason={}",
                    op,
                    fd,
                    session.thread_group_id,
                    match e {
                        ResolveError::BadFd => "unknown",
                        ResolveError::Stale => "stale_instance",
                    }
                );
                return Err(error(op, VFS_STATUS_ERR_BAD_FD, fd));
            }
        };
        match mounts.instance(t.backend.instance) {
            Some((cap, label)) if cap == t.backend.cap => Ok((t.backend, t.handle, label)),
            _ => {
                yarm_user_rt::user_log!(
                    "VFS_FD_BAD op={} fd={} tgid={} reason=instance_not_mounted instance={}",
                    op,
                    fd,
                    session.thread_group_id,
                    t.backend.instance
                );
                Err(error(op, VFS_STATUS_ERR_BAD_FD, fd))
            }
        }
    }

    // ── path operations ────────────────────────────────────────────────────────────────────

    /// Normalize once and route on the canonical path.
    fn route_path(
        mounts: &VfsMountTable,
        op: u16,
        raw: Option<&[u8]>,
    ) -> Result<(super::path::NormalizedPath, BackendRef, MountLabel), Message> {
        let Some(raw) = raw else {
            yarm_user_rt::user_log!("VFS_PATH_DECODE_REJECT op={}", op);
            return Err(error(op, VFS_STATUS_ERR_INVALID_PATH, 0));
        };
        let norm = match super::path::normalize(raw) {
            Ok(n) => n,
            Err(e) => {
                yarm_user_rt::user_log!("VFS_PATH_NORM_REJECT reason={}", e.as_str());
                return Err(error(op, VFS_STATUS_ERR_INVALID_PATH, 0));
            }
        };
        let Some((cap, instance, label)) = mounts.route_instance(norm.as_bytes()) else {
            yarm_user_rt::user_log!("VFS_ROUTE_NO_MOUNT path={}", path_str(norm.as_bytes()));
            return Err(error(op, VFS_STATUS_ERR_NO_MOUNT, 0));
        };
        yarm_user_rt::user_log!(
            "VFS_ROUTE_LOOKUP path={} target={} instance={} canonical={}",
            path_str(norm.as_bytes()),
            label.as_str(),
            instance,
            (norm.as_bytes() == raw) as u8
        );
        Ok((norm, BackendRef { instance, cap }, label))
    }

    fn statx<I: RouterIo>(&mut self, io: &mut I, mounts: &VfsMountTable, msg: &Message) -> Message {
        let args = StatxInlinePath::decode(msg.as_slice());
        let (norm, backend, label) =
            match Self::route_path(mounts, VFS_OP_STATX, args.map(|a| a.path)) {
                Ok(r) => r,
                Err(reply) => return reply,
            };
        let args = args.expect("decoded above");
        if args.dirfd != 0 {
            yarm_user_rt::user_log!("VFS_DIRFD_IGNORED op={} dirfd={}", VFS_OP_STATX, args.dirfd);
        }
        // The path is absolute, so the dirfd does not take part; a client fd means nothing to a
        // backend, so it is not forwarded. The flags and mask go as sent.
        let (bytes, len) = StatxInlinePath {
            dirfd: 0,
            flags: args.flags,
            mask_or_buf: args.mask_or_buf,
            path: norm.as_bytes(),
        }
        .encode()
        .expect("a canonical path is never longer than the one decoded");
        let expect = VfsCompletionExpect {
            op: VFS_OP_STATX,
            fd: 0,
            requested: 0,
        };
        match Self::forward(
            io,
            backend,
            label.as_str(),
            &request(VFS_OP_STATX, &bytes[..len]),
            expect,
        ) {
            Forwarded::Ok { value, .. } => {
                completion(VfsCompletion::ok(VFS_OP_STATX, value, 0), &[], None)
            }
            Forwarded::Status(s) => error(VFS_OP_STATX, s, 0),
            Forwarded::Malformed => error(VFS_OP_STATX, VFS_STATUS_ERR_BACKEND_MALFORMED, 0),
            Forwarded::NotDelivered | Forwarded::NoReply => {
                error(VFS_OP_STATX, VFS_STATUS_ERR_BACKEND, 0)
            }
        }
    }

    fn openat<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        session: SessionId,
        caller_tid: u64,
        msg: &Message,
        reply_cap: u64,
    ) {
        let args = OpenAtInlinePath::decode(msg.as_slice());
        let (norm, backend, label) =
            match Self::route_path(mounts, VFS_OP_OPENAT, args.map(|a| a.path)) {
                Ok(r) => r,
                Err(reply) => {
                    let _ = io.reply_client(reply_cap, &reply);
                    return;
                }
            };
        let args = args.expect("decoded above");
        if args.dirfd != 0 {
            yarm_user_rt::user_log!(
                "VFS_DIRFD_IGNORED op={} dirfd={}",
                VFS_OP_OPENAT,
                args.dirfd
            );
        }
        // Capacity first: a full table answers before anything is opened at a backend.
        let r = match self.fds.reserve(session, backend) {
            Ok(r) => r,
            Err(e) => {
                yarm_user_rt::user_log!(
                    "VFS_FD_NO_FREE tgid={} incarnation={} reason={} status={}",
                    session.thread_group_id,
                    session.incarnation,
                    match e {
                        ReserveError::SessionFull => "session_full",
                        ReserveError::TableFull => "table_full",
                    },
                    VFS_STATUS_ERR_NO_FD
                );
                let _ = io.reply_client(reply_cap, &error(VFS_OP_OPENAT, VFS_STATUS_ERR_NO_FD, 0));
                return;
            }
        };
        let (bytes, len) = OpenAtInlinePath {
            dirfd: 0,
            flags: args.flags,
            mode: args.mode,
            path: norm.as_bytes(),
        }
        .encode()
        .expect("a canonical path is never longer than the one decoded");
        let expect = VfsCompletionExpect {
            op: VFS_OP_OPENAT,
            fd: 0,
            requested: 0,
        };
        let handle = match Self::forward(
            io,
            backend,
            label.as_str(),
            &request(VFS_OP_OPENAT, &bytes[..len]),
            expect,
        ) {
            Forwarded::Ok { value, .. } => value,
            other => {
                self.fds.abort(r);
                let status = match other {
                    Forwarded::Status(s) => s,
                    Forwarded::Malformed => {
                        self.unknown_backend_opens += 1;
                        yarm_user_rt::user_log!(
                            "VFS_OPEN_BACKEND_STATE_UNKNOWN reason=malformed instance={} unknown_backend_opens={}",
                            backend.instance,
                            self.unknown_backend_opens
                        );
                        VFS_STATUS_ERR_BACKEND_MALFORMED
                    }
                    Forwarded::NoReply => {
                        self.unknown_backend_opens += 1;
                        yarm_user_rt::user_log!(
                            "VFS_OPEN_BACKEND_STATE_UNKNOWN reason=no_reply instance={} unknown_backend_opens={}",
                            backend.instance,
                            self.unknown_backend_opens
                        );
                        VFS_STATUS_ERR_BACKEND
                    }
                    _ => VFS_STATUS_ERR_BACKEND,
                };
                let _ = io.reply_client(reply_cap, &error(VFS_OP_OPENAT, status, 0));
                return;
            }
        };
        let fd = self.fds.commit(r, handle);
        yarm_user_rt::user_log!(
            "VFS_FD_OPEN tgid={} incarnation={} tid={} client_fd={} instance={} target={} backend_handle={}",
            session.thread_group_id,
            session.incarnation,
            caller_tid,
            fd,
            backend.instance,
            label.as_str(),
            handle
        );
        let inject = self.fail_reply_path == Some(norm.as_bytes());
        let target_cap = if inject {
            reply_cap.wrapping_add(1 << 16)
        } else {
            reply_cap
        };
        let reply = completion(VfsCompletion::ok(VFS_OP_OPENAT, fd, 0), &[], None);
        if io.reply_client(target_cap, &reply) {
            self.log_state("open");
            return;
        }
        // The client was not told: nothing may keep the backend handle.
        let closed = match self.fds.close(session, fd) {
            Ok(Some(c)) => {
                let r = self.backend_close(io, mounts, c);
                self.fds.close_settled(c, r.outcome);
                Some(r.outcome)
            }
            _ => None,
        };
        yarm_user_rt::user_log!(
            "VFS_OPEN_ROLLBACK tgid={} client_fd={} instance={} backend_handle={} injected={} close={:?}",
            session.thread_group_id,
            fd,
            backend.instance,
            handle,
            inject as u8,
            closed
        );
        self.log_state("rollback");
        if inject {
            let _ = io.reply_client(reply_cap, &error(VFS_OP_OPENAT, VFS_STATUS_ERR_BACKEND, 0));
        }
    }

    // ── fd operations ──────────────────────────────────────────────────────────────────────

    fn read<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        session: SessionId,
        msg: &Message,
    ) -> Message {
        let Ok(args) = ReadWriteArgs::decode(msg.as_slice()) else {
            return error(VFS_OP_READ, VFS_STATUS_ERR_CODEC, 0);
        };
        let (backend, handle, label) = match self.target(mounts, session, VFS_OP_READ, args.fd) {
            Ok(t) => t,
            Err(reply) => return reply,
        };
        let req = request(
            VFS_OP_READ,
            &ReadWriteArgs::new(handle, args.buf_ptr, args.len).encode(),
        );
        let expect = VfsCompletionExpect {
            op: VFS_OP_READ,
            fd: handle,
            requested: args.len,
        };
        match Self::forward(io, backend, label.as_str(), &req, expect) {
            Forwarded::Ok {
                value, data, len, ..
            } => completion(
                VfsCompletion::ok(VFS_OP_READ, value, args.fd),
                &data[..len],
                None,
            ),
            Forwarded::Status(s) => error(VFS_OP_READ, s, args.fd),
            Forwarded::Malformed => error(VFS_OP_READ, VFS_STATUS_ERR_BACKEND_MALFORMED, args.fd),
            Forwarded::NotDelivered | Forwarded::NoReply => {
                error(VFS_OP_READ, VFS_STATUS_ERR_BACKEND, args.fd)
            }
        }
    }

    fn grant<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        session: SessionId,
        msg: &Message,
    ) -> Message {
        let Ok(args) = FileGrantRoArgs::decode(msg.as_slice()) else {
            return error(VFS_OP_FILE_GRANT_RO, VFS_STATUS_ERR_CODEC, 0);
        };
        let (backend, handle, label) =
            match self.target(mounts, session, VFS_OP_FILE_GRANT_RO, args.fd) {
                Ok(t) => t,
                Err(reply) => return reply,
            };
        let req = request(
            VFS_OP_FILE_GRANT_RO,
            &FileGrantRoArgs { fd: handle, ..args }.encode(),
        );
        let expect = VfsCompletionExpect {
            op: VFS_OP_FILE_GRANT_RO,
            fd: handle,
            requested: 0,
        };
        match Self::forward(io, backend, label.as_str(), &req, expect) {
            Forwarded::Ok { value, cap, .. } => {
                yarm_user_rt::user_log!(
                    "VFS_FILE_GRANT_RO_RELAY client_fd={} backend_handle={} has_cap={}",
                    args.fd,
                    handle,
                    cap.is_some() as u8
                );
                completion(
                    VfsCompletion::ok(VFS_OP_FILE_GRANT_RO, value, args.fd),
                    &[],
                    cap,
                )
            }
            Forwarded::Status(s) => error(VFS_OP_FILE_GRANT_RO, s, args.fd),
            Forwarded::Malformed => error(
                VFS_OP_FILE_GRANT_RO,
                VFS_STATUS_ERR_BACKEND_MALFORMED,
                args.fd,
            ),
            Forwarded::NotDelivered | Forwarded::NoReply => {
                error(VFS_OP_FILE_GRANT_RO, VFS_STATUS_ERR_BACKEND, args.fd)
            }
        }
    }

    /// The descriptor goes whatever the backend says (a failed close still releases the fd).
    fn close<I: RouterIo>(
        &mut self,
        io: &mut I,
        mounts: &VfsMountTable,
        session: SessionId,
        caller_tid: u64,
        msg: &Message,
    ) -> Message {
        let Ok(args) = ReadWriteArgs::decode(msg.as_slice()) else {
            return error(VFS_OP_CLOSE, VFS_STATUS_ERR_CODEC, 0);
        };
        let reply = match self.fds.close(session, args.fd) {
            Err(_) => {
                yarm_user_rt::user_log!(
                    "VFS_FD_BAD op={} fd={} tgid={} reason=unknown",
                    VFS_OP_CLOSE,
                    args.fd,
                    session.thread_group_id
                );
                return error(VFS_OP_CLOSE, VFS_STATUS_ERR_BAD_FD, args.fd);
            }
            // Nothing open at a live backend (a stale descriptor, or a shared description).
            Ok(None) => completion(VfsCompletion::ok(VFS_OP_CLOSE, 0, args.fd), &[], None),
            Ok(Some(c)) => {
                let r = self.backend_close(io, mounts, c);
                self.fds.close_settled(c, r.outcome);
                yarm_user_rt::user_log!(
                    "VFS_FD_CLOSE tgid={} tid={} client_fd={} instance={} backend_handle={} outcome={:?}",
                    session.thread_group_id,
                    caller_tid,
                    args.fd,
                    c.backend.instance,
                    c.handle,
                    r.outcome
                );
                match r.outcome {
                    CloseOutcome::Closed => {
                        completion(VfsCompletion::ok(VFS_OP_CLOSE, r.value, args.fd), &[], None)
                    }
                    CloseOutcome::Refused(s) => error(VFS_OP_CLOSE, s, args.fd),
                    CloseOutcome::NotDelivered | CloseOutcome::Ambiguous => {
                        error(VFS_OP_CLOSE, VFS_STATUS_ERR_BACKEND, args.fd)
                    }
                }
            }
        };
        self.log_state("close");
        reply
    }
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
