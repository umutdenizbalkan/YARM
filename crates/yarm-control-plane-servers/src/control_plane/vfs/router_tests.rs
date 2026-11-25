// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-CORRECT1C — the router against scripted backends and a scripted kernel. Each fake backend
//! numbers its handles from the same base, so every pair of opens on two backends collides.

use super::*;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use yarm_ipc_abi::vfs_abi::{VFS_OP_DUP, VFS_STATUS_OK, VfsCompletion};

const CAP_A: u64 = 0x1_0001_0005;
const CAP_B: u64 = 0x1_0001_0006;
const HANDLE_BASE: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// This call goes through untouched.
    Pass,
    NotDelivered,
    NoReply,
    /// A completion that names another fd than the one forwarded.
    WrongFd,
    /// The backend answers with this status.
    Status(u32),
}

struct FakeBackend {
    files: BTreeMap<Vec<u8>, Vec<u8>>,
    next: u64,
    open: BTreeMap<u64, (Vec<u8>, usize)>,
}

impl FakeBackend {
    fn new(files: &[(&[u8], &[u8])]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, b)| (p.to_vec(), b.to_vec()))
                .collect(),
            next: HANDLE_BASE,
            open: BTreeMap::new(),
        }
    }

    fn serve(&mut self, req: &Message) -> (Message, Option<u64>) {
        let ok =
            |op, value, fd, data: &[u8]| completion(VfsCompletion::ok(op, value, fd), data, None);
        match req.opcode {
            VFS_OP_OPENAT => {
                let a = OpenAtInlinePath::decode(req.as_slice()).unwrap();
                match self.files.get(a.path) {
                    Some(_) => {
                        let h = self.next;
                        self.next += 1;
                        self.open.insert(h, (a.path.to_vec(), 0));
                        (ok(VFS_OP_OPENAT, h, 0, &[]), None)
                    }
                    None => (error(VFS_OP_OPENAT, VFS_STATUS_ERR_INVALID_PATH, 0), None),
                }
            }
            VFS_OP_STATX => {
                let a = StatxInlinePath::decode(req.as_slice()).unwrap();
                match self.files.get(a.path) {
                    Some(b) => (ok(VFS_OP_STATX, b.len() as u64, 0, &[]), None),
                    None => (error(VFS_OP_STATX, VFS_STATUS_ERR_INVALID_PATH, 0), None),
                }
            }
            VFS_OP_READ => {
                let a = ReadWriteArgs::decode(req.as_slice()).unwrap();
                let Some((path, off)) = self.open.get_mut(&a.fd) else {
                    return (error(VFS_OP_READ, VFS_STATUS_ERR_BAD_FD, a.fd), None);
                };
                let bytes = &self.files[path.as_slice()];
                let n = (bytes.len() - *off)
                    .min(a.len as usize)
                    .min(VFS_INLINE_READ_MAX);
                let out = bytes[*off..*off + n].to_vec();
                *off += n;
                (ok(VFS_OP_READ, n as u64, a.fd, &out), None)
            }
            VFS_OP_CLOSE => {
                let a = ReadWriteArgs::decode(req.as_slice()).unwrap();
                match self.open.remove(&a.fd) {
                    Some(_) => (ok(VFS_OP_CLOSE, 0, a.fd, &[]), None),
                    None => (error(VFS_OP_CLOSE, VFS_STATUS_ERR_BAD_FD, a.fd), None),
                }
            }
            VFS_OP_FILE_GRANT_RO => {
                let a = FileGrantRoArgs::decode(req.as_slice()).unwrap();
                match self.open.get(&a.fd) {
                    Some((p, _)) => {
                        let len = self.files[p.as_slice()].len() as u64;
                        let cap = 0x9_0000 + a.fd;
                        (
                            completion(
                                VfsCompletion::ok(VFS_OP_FILE_GRANT_RO, len, a.fd),
                                &[],
                                Some(cap),
                            ),
                            Some(cap),
                        )
                    }
                    None => (
                        error(VFS_OP_FILE_GRANT_RO, VFS_STATUS_ERR_BAD_FD, a.fd),
                        None,
                    ),
                }
            }
            op => (error(op, VFS_STATUS_ERR_UNKNOWN_OP, 0), None),
        }
    }
}

struct Call {
    cap: u64,
    op: u16,
    payload: Vec<u8>,
}

struct FakeIo {
    backends: BTreeMap<u64, FakeBackend>,
    calls: Vec<Call>,
    faults: VecDeque<Fault>,
    /// What each reply cap's caller is: (session, tid).
    callers: BTreeMap<u64, (SessionId, u64)>,
    dead: BTreeSet<(u64, u64)>,
    refuse_reply: BTreeSet<u64>,
    replies: Vec<(u64, Message)>,
}

impl RouterIo for FakeIo {
    fn call_backend(&mut self, backend: BackendRef, _label: &str, request: &Message) -> CallResult {
        self.calls.push(Call {
            cap: backend.cap,
            op: request.opcode,
            payload: request.as_slice().to_vec(),
        });
        let fault = self.faults.pop_front();
        if fault == Some(Fault::NotDelivered) {
            return CallResult::NotDelivered;
        }
        let b = self.backends.get_mut(&backend.cap).expect("known backend");
        let (mut msg, cap) = b.serve(request);
        match fault {
            Some(Fault::NoReply) => return CallResult::NoReply,
            Some(Fault::WrongFd) => {
                let (c, data) = VfsCompletion::decode(msg.as_slice()).unwrap();
                let data = data.to_vec();
                msg = completion(
                    VfsCompletion {
                        fd: c.fd ^ 0x40,
                        ..c
                    },
                    &data,
                    cap,
                );
            }
            Some(Fault::Status(s)) => {
                let (c, _) = VfsCompletion::decode(msg.as_slice()).unwrap();
                msg = error(request.opcode, s, c.fd);
                return CallResult::Reply {
                    message: msg,
                    has_cap: false,
                    transferred_cap: None,
                };
            }
            _ => {}
        }
        CallResult::Reply {
            message: msg,
            has_cap: cap.is_some(),
            transferred_cap: cap,
        }
    }

    fn reply_client(&mut self, reply_cap: u64, reply: &Message) -> bool {
        if self.refuse_reply.contains(&reply_cap) || !self.callers.contains_key(&reply_cap) {
            return false;
        }
        self.replies.push((reply_cap, *reply));
        true
    }

    fn caller_session(&mut self, reply_cap: u64) -> Option<(SessionId, u64)> {
        self.callers.get(&reply_cap).copied()
    }

    fn session_alive(&mut self, s: SessionId) -> Option<bool> {
        Some(!self.dead.contains(&(s.thread_group_id, s.incarnation)))
    }
}

const P1: SessionId = SessionId {
    thread_group_id: 40,
    incarnation: 7,
};
const P2: SessionId = SessionId {
    thread_group_id: 41,
    incarnation: 9,
};
const FILE_A: &[u8] = b"/a/f";
const FILE_B: &[u8] = b"/b/f";

fn bytes_a() -> Vec<u8> {
    (0..200u32).map(|i| (i * 7 + 1) as u8).collect()
}
fn bytes_b() -> Vec<u8> {
    (0..200u32).map(|i| (i * 13 + 5) as u8).collect()
}

struct World {
    io: FakeIo,
    mounts: VfsMountTable,
    router: Router,
    next_reply: u64,
}

impl World {
    fn new() -> Self {
        let a = bytes_a();
        let b = bytes_b();
        let mut backends = BTreeMap::new();
        backends.insert(
            CAP_A,
            FakeBackend::new(&[(FILE_A, &a), (b"/a", b""), (b"/a/bx", b"x")]),
        );
        backends.insert(
            CAP_B,
            FakeBackend::new(&[(FILE_B, &b), (b"/a/b/x", b"y"), (b"/a/b", b"")]),
        );
        let mut mounts = VfsMountTable::new();
        assert!(mounts.register(b"/a/", "a", CAP_A));
        assert!(mounts.register(b"/b/", "b", CAP_B));
        Self {
            io: FakeIo {
                backends,
                calls: Vec::new(),
                faults: VecDeque::new(),
                callers: BTreeMap::new(),
                dead: BTreeSet::new(),
                refuse_reply: BTreeSet::new(),
                replies: Vec::new(),
            },
            mounts,
            router: Router::new(),
            next_reply: 0x2_0000_0100,
        }
    }

    /// One request from `tid` of `s`; returns the completion the client got (if any).
    fn send(
        &mut self,
        s: SessionId,
        tid: u64,
        msg: Message,
    ) -> Option<(VfsCompletion, Vec<u8>, Option<u64>)> {
        let rc = self.next_reply;
        self.next_reply += 1;
        self.io.callers.insert(rc, (s, tid));
        let before = self.io.replies.len();
        self.router.handle(&mut self.io, &self.mounts, &msg, rc);
        let r = self.io.replies[before..]
            .iter()
            .rev()
            .find(|(c, _)| *c == rc)?;
        let (c, data) = VfsCompletion::decode(r.1.as_slice()).unwrap();
        Some((c, data.to_vec(), r.1.transferred_cap().map(|c| c.0)))
    }

    fn open(&mut self, s: SessionId, path: &[u8]) -> Result<u64, u32> {
        self.open_with(s, 0, path, 0, 0)
    }

    fn open_with(
        &mut self,
        s: SessionId,
        dirfd: u64,
        path: &[u8],
        flags: u64,
        mode: u64,
    ) -> Result<u64, u32> {
        let (b, n) = OpenAtInlinePath {
            dirfd,
            flags,
            mode,
            path,
        }
        .encode()
        .unwrap();
        let (c, _, _) = self
            .send(s, s.thread_group_id, request(VFS_OP_OPENAT, &b[..n]))
            .unwrap();
        assert_eq!(c.op, VFS_OP_OPENAT);
        assert_eq!(c.fd, 0);
        if c.status == VFS_STATUS_OK {
            Ok(c.value)
        } else {
            Err(c.status)
        }
    }

    fn statx(&mut self, s: SessionId, path: &[u8]) -> Result<u64, u32> {
        let (b, n) = StatxInlinePath {
            dirfd: 0,
            flags: 0x100,
            mask_or_buf: 0x7ff,
            path,
        }
        .encode()
        .unwrap();
        let (c, _, _) = self
            .send(s, s.thread_group_id, request(VFS_OP_STATX, &b[..n]))
            .unwrap();
        if c.status == VFS_STATUS_OK {
            Ok(c.value)
        } else {
            Err(c.status)
        }
    }

    fn read_as(&mut self, s: SessionId, tid: u64, fd: u64, len: u64) -> Result<Vec<u8>, u32> {
        let (c, data, _) = self
            .send(
                s,
                tid,
                request(VFS_OP_READ, &ReadWriteArgs::new(fd, 0, len).encode()),
            )
            .unwrap();
        assert_eq!(c.op, VFS_OP_READ);
        assert_eq!(c.fd, fd, "the client completion names the client fd");
        if c.status == VFS_STATUS_OK {
            assert_eq!(c.value as usize, data.len());
            Ok(data)
        } else {
            Err(c.status)
        }
    }

    fn read(&mut self, s: SessionId, fd: u64, len: u64) -> Result<Vec<u8>, u32> {
        self.read_as(s, s.thread_group_id, fd, len)
    }

    fn close_as(&mut self, s: SessionId, tid: u64, fd: u64) -> Result<(), u32> {
        let (c, _, _) = self
            .send(
                s,
                tid,
                request(VFS_OP_CLOSE, &ReadWriteArgs::new(fd, 0, 0).encode()),
            )
            .unwrap();
        assert_eq!(c.fd, fd);
        if c.status == VFS_STATUS_OK {
            Ok(())
        } else {
            Err(c.status)
        }
    }

    fn close(&mut self, s: SessionId, fd: u64) -> Result<(), u32> {
        self.close_as(s, s.thread_group_id, fd)
    }

    fn backend_open(&self, cap: u64) -> usize {
        self.io.backends[&cap].open.len()
    }

    fn calls(&self, op: u16) -> usize {
        self.io.calls.iter().filter(|c| c.op == op).count()
    }

    fn last_path(&self, op: u16) -> Vec<u8> {
        let c = self.io.calls.iter().rev().find(|c| c.op == op).unwrap();
        if op == VFS_OP_OPENAT {
            OpenAtInlinePath::decode(&c.payload).unwrap().path.to_vec()
        } else {
            StatxInlinePath::decode(&c.payload).unwrap().path.to_vec()
        }
    }

    fn zero(&self) -> bool {
        let c = self.router.counts();
        c.descriptors == 0 && c.ofds == 0 && c.pending_retry == 0 && c.quarantined == 0
    }
}

fn handles_forwarded(w: &World, op: u16) -> Vec<(u64, u64)> {
    w.io.calls
        .iter()
        .filter(|c| c.op == op)
        .map(|c| (c.cap, ReadWriteArgs::decode(&c.payload).unwrap().fd))
        .collect()
}

// ── F01: the client fd namespace ──────────────────────────────────────────────────────────────

#[test]
fn vc1c_two_backends_same_handle_two_fds_right_bytes_close_one_keep_other() {
    let mut w = World::new();
    let fa = w.open(P1, FILE_A).unwrap();
    let fb = w.open(P1, FILE_B).unwrap();
    // Both backends answered handle 100.
    assert_eq!(
        w.io.backends[&CAP_A]
            .open
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [HANDLE_BASE]
    );
    assert_eq!(
        w.io.backends[&CAP_B]
            .open
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [HANDLE_BASE]
    );
    assert_ne!(fa, fb, "two open files, two client fds");
    assert_ne!(fa, HANDLE_BASE);
    assert_ne!(fb, HANDLE_BASE);
    let a = bytes_a();
    let b = bytes_b();
    assert_eq!(w.read(P1, fa, 16).unwrap(), a[..16]);
    assert_eq!(w.read(P1, fb, 16).unwrap(), b[..16]);
    assert_eq!(w.read(P1, fb, 8).unwrap(), b[16..24]);
    w.close(P1, fa).unwrap();
    assert_eq!(w.backend_open(CAP_A), 0);
    assert_eq!(w.backend_open(CAP_B), 1);
    // fb continues at its offset, from its own backend.
    assert_eq!(w.read(P1, fb, 8).unwrap(), b[24..32]);
    assert_eq!(w.read(P1, fa, 8), Err(VFS_STATUS_ERR_BAD_FD));
    // Every READ went to the right backend with the backend handle, never a client fd.
    assert_eq!(
        handles_forwarded(&w, VFS_OP_READ),
        [
            (CAP_A, HANDLE_BASE),
            (CAP_B, HANDLE_BASE),
            (CAP_B, HANDLE_BASE),
            (CAP_B, HANDLE_BASE)
        ]
    );
    assert_eq!(handles_forwarded(&w, VFS_OP_CLOSE), [(CAP_A, HANDLE_BASE)]);
    w.close(P1, fb).unwrap();
    assert!(w.zero());
}

#[test]
fn vc1c_backend_completion_is_validated_against_the_forwarded_handle() {
    let mut w = World::new();
    let fa = w.open(P1, FILE_A).unwrap();
    w.io.faults.push_back(Fault::WrongFd);
    assert_eq!(w.read(P1, fa, 8), Err(VFS_STATUS_ERR_BACKEND_MALFORMED));
    // A backend that echoes the CLIENT fd (as a client-fd-forwarding router would expect) is
    // malformed too: the forwarded fd was the handle.
    assert_ne!(fa, HANDLE_BASE);
    assert_eq!(w.read(P1, fa, 8).unwrap(), bytes_a()[8..16]);
}

#[test]
fn vc1c_file_grant_ro_translates_the_fd_and_keeps_the_capability() {
    let mut w = World::new();
    let _ = w.open(P1, FILE_B).unwrap();
    let fa = w.open(P1, FILE_A).unwrap();
    let (c, data, cap) = w
        .send(
            P1,
            40,
            request(VFS_OP_FILE_GRANT_RO, &FileGrantRoArgs::new(fa).encode()),
        )
        .unwrap();
    assert_eq!(
        (c.op, c.status, c.value, c.fd),
        (VFS_OP_FILE_GRANT_RO, VFS_STATUS_OK, 200, fa)
    );
    assert!(data.is_empty());
    assert_eq!(cap, Some(0x9_0000 + HANDLE_BASE));
    let last = w.io.calls.last().unwrap();
    assert_eq!(
        (last.cap, FileGrantRoArgs::decode(&last.payload).unwrap().fd),
        (CAP_A, HANDLE_BASE)
    );
    let r = &w.io.replies.last().unwrap().1;
    assert_eq!(
        r.flags & Message::FLAG_CAP_TRANSFER,
        Message::FLAG_CAP_TRANSFER
    );
}

#[test]
fn vc1c_retired_and_unknown_fd_operations_are_not_forwarded() {
    let mut w = World::new();
    let fa = w.open(P1, FILE_A).unwrap();
    let n = w.io.calls.len();
    let bulk = yarm_ipc_abi::vfs_abi::BulkReadArgs::new(fa, 4096, 0).encode();
    let (c, _, _) = w.send(P1, 40, request(VFS_OP_READ_BULK, &bulk)).unwrap();
    assert_eq!((c.status, c.fd), (VFS_STATUS_ERR_UNSUPPORTED, fa));
    let (c, _, _) = w
        .send(
            P1,
            40,
            request(VFS_OP_DUP, &ReadWriteArgs::new(fa, 0, 0).encode()),
        )
        .unwrap();
    assert_eq!(c.status, VFS_STATUS_ERR_UNKNOWN_OP);
    assert_eq!(w.io.calls.len(), n);
}

#[test]
fn vc1c_bad_fd_and_double_close_are_not_forwarded() {
    let mut w = World::new();
    assert_eq!(w.read(P1, 3, 1), Err(VFS_STATUS_ERR_BAD_FD));
    assert_eq!(w.close(P1, 3), Err(VFS_STATUS_ERR_BAD_FD));
    let fa = w.open(P1, FILE_A).unwrap();
    w.close(P1, fa).unwrap();
    assert_eq!(w.close(P1, fa), Err(VFS_STATUS_ERR_BAD_FD));
    assert_eq!(w.calls(VFS_OP_CLOSE), 1);
    assert_eq!(w.calls(VFS_OP_READ), 0);
}

#[test]
fn vc1c_unauthenticated_caller_is_refused_before_anything_is_forwarded() {
    let mut w = World::new();
    let (b, n) = OpenAtInlinePath {
        dirfd: 0,
        flags: 0,
        mode: 0,
        path: FILE_A,
    }
    .encode()
    .unwrap();
    // A reply cap the kernel names no caller for: the fake refuses the session and the reply.
    w.router.handle(
        &mut w.io,
        &w.mounts,
        &request(VFS_OP_OPENAT, &b[..n]),
        0xdead,
    );
    assert!(w.io.calls.is_empty());
    assert!(w.zero());
}

// ── F02: recoverable open and close ───────────────────────────────────────────────────────────

#[test]
fn vc1c_full_session_refuses_no_fd_without_opening_at_the_backend() {
    let mut w = World::new();
    let mut fds = Vec::new();
    for _ in 0..yarm_fs_servers::common::fd_space::MAX_FDS_PER_SESSION {
        fds.push(w.open(P1, FILE_A).unwrap());
    }
    let opens = w.calls(VFS_OP_OPENAT);
    let live = w.backend_open(CAP_A);
    assert_eq!(w.open(P1, FILE_B), Err(VFS_STATUS_ERR_NO_FD));
    assert_eq!(
        w.calls(VFS_OP_OPENAT),
        opens,
        "nothing was opened at a backend"
    );
    assert_eq!((w.backend_open(CAP_A), w.backend_open(CAP_B)), (live, 0));
    // Another session still has room; one close frees a slot here.
    assert!(w.open(P2, FILE_B).is_ok());
    w.close(P1, fds[3]).unwrap();
    assert_eq!(
        w.open(P1, FILE_B),
        Ok(fds[3]),
        "the lowest free number is reused"
    );
}

#[test]
fn vc1c_full_table_refuses_no_fd_without_opening_at_the_backend() {
    let mut w = World::new();
    let sessions: Vec<SessionId> = (0..3)
        .map(|i| SessionId {
            thread_group_id: 100 + i,
            incarnation: 1,
        })
        .collect();
    let mut total = 0;
    'fill: for s in &sessions {
        for _ in 0..yarm_fs_servers::common::fd_space::MAX_FDS_PER_SESSION {
            if total == yarm_fs_servers::common::fd_space::MAX_DESCRIPTORS {
                break 'fill;
            }
            w.open(*s, FILE_A).unwrap();
            total += 1;
        }
    }
    let opens = w.calls(VFS_OP_OPENAT);
    assert_eq!(w.open(sessions[2], FILE_A), Err(VFS_STATUS_ERR_NO_FD));
    assert_eq!(w.calls(VFS_OP_OPENAT), opens);
    assert_eq!(w.backend_open(CAP_A), 32);
}

#[test]
fn vc1c_backend_refusal_releases_the_reservation() {
    let mut w = World::new();
    assert_eq!(w.open(P1, b"/a/missing"), Err(VFS_STATUS_ERR_INVALID_PATH));
    assert!(w.zero());
    w.io.faults.push_back(Fault::NotDelivered);
    assert_eq!(w.open(P1, FILE_A), Err(VFS_STATUS_ERR_BACKEND));
    assert!(w.zero());
    assert_eq!(w.router.unknown_backend_opens, 0);
    assert_eq!(w.open(P1, FILE_A), Ok(3));
}

#[test]
fn vc1c_failed_client_reply_closes_the_exact_backend_handle() {
    let mut w = World::new();
    let fb = w.open(P1, FILE_B).unwrap(); // CAP_B holds handle 100 too
    // The next reply cap the world hands out is refused by the kernel.
    w.io.refuse_reply.insert(w.next_reply);
    let (b, n) = OpenAtInlinePath {
        dirfd: 0,
        flags: 0,
        mode: 0,
        path: FILE_A,
    }
    .encode()
    .unwrap();
    assert!(w.send(P1, 40, request(VFS_OP_OPENAT, &b[..n])).is_none());
    assert_eq!(
        w.backend_open(CAP_A),
        0,
        "the rollback closed the handle the backend opened"
    );
    assert_eq!(
        w.backend_open(CAP_B),
        1,
        "and only that one, at that instance"
    );
    assert_eq!(handles_forwarded(&w, VFS_OP_CLOSE), [(CAP_A, HANDLE_BASE)]);
    let c = w.router.counts();
    assert_eq!((c.descriptors, c.ofds), (1, 1));
    assert_eq!(w.read(P1, fb, 4).unwrap(), bytes_b()[..4]);
}

#[test]
fn vc1c_injected_fail_reply_rolls_back_then_answers_backend() {
    let mut w = World::new();
    w.router.fail_reply_path = Some(FILE_A);
    let base = w.next_reply;
    // The stale cap (next generation) is unknown to the kernel: refused.
    let (b, n) = OpenAtInlinePath {
        dirfd: 0,
        flags: 0,
        mode: 0,
        path: b"/a//./f",
    }
    .encode()
    .unwrap();
    let (c, _, _) = w.send(P1, 40, request(VFS_OP_OPENAT, &b[..n])).unwrap();
    assert_eq!((c.status, c.value), (VFS_STATUS_ERR_BACKEND, 0));
    assert!(!w.io.replies.iter().any(|(cap, _)| *cap == base + (1 << 16)));
    assert_eq!(w.backend_open(CAP_A), 0);
    assert!(w.zero());
}

#[test]
fn vc1c_rollback_close_not_delivered_is_retried_at_the_same_instance() {
    let mut w = World::new();
    w.io.refuse_reply.insert(w.next_reply);
    // The OPENAT passes; the compensating CLOSE is not delivered.
    w.io.faults.extend([Fault::Pass, Fault::NotDelivered]);
    let (b, n) = OpenAtInlinePath {
        dirfd: 0,
        flags: 0,
        mode: 0,
        path: FILE_A,
    }
    .encode()
    .unwrap();
    assert!(w.send(P1, 40, request(VFS_OP_OPENAT, &b[..n])).is_none());
    assert_eq!(
        w.backend_open(CAP_A),
        1,
        "the undelivered close left the handle open"
    );
    let c = w.router.counts();
    assert_eq!((c.descriptors, c.ofds, c.pending_retry), (0, 0, 1));
    // The next request from anyone retries it, at CAP_A, with that handle.
    let _ = w.statx(P2, FILE_B);
    assert_eq!(w.backend_open(CAP_A), 0);
    assert_eq!(
        handles_forwarded(&w, VFS_OP_CLOSE),
        [(CAP_A, HANDLE_BASE), (CAP_A, HANDLE_BASE)]
    );
    assert!(w.zero());
}

#[test]
fn vc1c_ambiguous_close_is_quarantined_and_never_retried() {
    let mut w = World::new();
    let fa = w.open(P1, FILE_A).unwrap();
    w.io.faults.push_back(Fault::NoReply);
    assert_eq!(w.close(P1, fa), Err(VFS_STATUS_ERR_BACKEND));
    let c = w.router.counts();
    assert_eq!(
        (c.descriptors, c.ofds, c.pending_retry, c.quarantined),
        (0, 0, 0, 1)
    );
    let closes = w.calls(VFS_OP_CLOSE);
    // The backend reuses the number for someone else; nothing ever closes it on our behalf.
    let fb = w.open(P2, b"/a/bx").unwrap();
    for _ in 0..4 {
        let _ = w.statx(P1, FILE_A);
    }
    assert_eq!(w.calls(VFS_OP_CLOSE), closes);
    assert_eq!(w.read(P2, fb, 1).unwrap(), b"x");
}

#[test]
fn vc1c_malformed_or_missing_open_reply_is_counted_not_guessed() {
    let mut w = World::new();
    w.io.faults.push_back(Fault::WrongFd);
    assert_eq!(w.open(P1, FILE_A), Err(VFS_STATUS_ERR_BACKEND_MALFORMED));
    w.io.faults.push_back(Fault::NoReply);
    assert_eq!(w.open(P1, FILE_A), Err(VFS_STATUS_ERR_BACKEND));
    assert_eq!(w.router.unknown_backend_opens, 2);
    assert!(w.zero());
    assert_eq!(w.calls(VFS_OP_CLOSE), 0, "no close by guess");
}

#[test]
fn vc1c_backend_refused_close_still_releases_the_descriptor() {
    let mut w = World::new();
    let fa = w.open(P1, FILE_A).unwrap();
    w.io.faults.push_back(Fault::Status(VFS_STATUS_ERR_BACKEND));
    assert_eq!(w.close(P1, fa), Err(VFS_STATUS_ERR_BACKEND));
    assert!(w.zero());
    assert_eq!(w.read(P1, fa, 1), Err(VFS_STATUS_ERR_BAD_FD));
}

// ── F03: sessions ─────────────────────────────────────────────────────────────────────────────

#[test]
fn vc1c_threads_of_one_session_share_descriptors() {
    let mut w = World::new();
    // Thread 50 of P1 opens; thread 51 of P1 reads and closes.
    let (b, n) = OpenAtInlinePath {
        dirfd: 0,
        flags: 0,
        mode: 0,
        path: FILE_A,
    }
    .encode()
    .unwrap();
    let (c, _, _) = w.send(P1, 50, request(VFS_OP_OPENAT, &b[..n])).unwrap();
    let fd = c.value;
    assert_eq!(w.read_as(P1, 51, fd, 4).unwrap(), bytes_a()[..4]);
    w.close_as(P1, 51, fd).unwrap();
    assert!(w.zero());
}

#[test]
fn vc1c_separate_sessions_same_number_resolve_only_their_own_file() {
    let mut w = World::new();
    let f1 = w.open(P1, FILE_A).unwrap();
    let f2 = w.open(P2, FILE_B).unwrap();
    assert_eq!(f1, f2, "each session numbers from 3");
    assert_eq!(w.read(P1, f1, 4).unwrap(), bytes_a()[..4]);
    assert_eq!(w.read(P2, f2, 4).unwrap(), bytes_b()[..4]);
    w.close(P1, f1).unwrap();
    assert_eq!(w.read(P2, f2, 4).unwrap(), bytes_b()[4..8]);
    assert_eq!(w.backend_open(CAP_B), 1);
}

#[test]
fn vc1c_reused_tgid_with_new_incarnation_sees_nothing() {
    let mut w = World::new();
    let f = w.open(P1, FILE_A).unwrap();
    let reborn = SessionId {
        thread_group_id: P1.thread_group_id,
        incarnation: P1.incarnation + 100,
    };
    assert_eq!(w.read(reborn, f, 1), Err(VFS_STATUS_ERR_BAD_FD));
    assert_eq!(w.close(reborn, f), Err(VFS_STATUS_ERR_BAD_FD));
}

#[test]
fn vc1c_exited_session_is_reclaimed_live_one_is_not() {
    let mut w = World::new();
    let _ = w.open(P2, FILE_A).unwrap();
    let _ = w.open(P2, FILE_B).unwrap();
    let keep = w.open(P1, FILE_B).unwrap();
    // P1's other thread exited, P1 lives: nothing happens to it.
    let _ = w.statx(P2, FILE_A);
    assert_eq!(w.router.counts().descriptors, 3);
    // P2's last thread exits; the next request from anyone reclaims it.
    w.io.dead.insert((P2.thread_group_id, P2.incarnation));
    let _ = w.statx(P1, FILE_A);
    let c = w.router.counts();
    assert_eq!((c.descriptors, c.ofds, c.sessions), (1, 1, 1));
    assert_eq!((w.backend_open(CAP_A), w.backend_open(CAP_B)), (0, 1));
    assert_eq!(w.read(P1, keep, 2).unwrap(), bytes_b()[..2]);
    w.io.dead.insert((P1.thread_group_id, P1.incarnation));
    let _ = w.statx(P2, FILE_A); // P2's tgid is not reused here; any caller sweeps
    assert!(w.zero());
    assert_eq!((w.backend_open(CAP_A), w.backend_open(CAP_B)), (0, 0));
}

#[test]
fn vc1c_backend_restart_never_addresses_the_new_backends_reused_handle() {
    let mut w = World::new();
    let old = w.open(P1, FILE_A).unwrap();
    // CAP_A's server restarts: a fresh backend (handles from 100 again) under a new capability.
    const CAP_A2: u64 = 0x1_0002_0005;
    w.io.backends
        .insert(CAP_A2, FakeBackend::new(&[(FILE_A, &bytes_b())]));
    let (gone, _) = w.mounts.replace_backend(b"/a/", CAP_A2).unwrap();
    let reads = w.calls(VFS_OP_READ);
    // Without being told, the router already refuses: the registration changed.
    assert_eq!(w.read(P1, old, 4), Err(VFS_STATUS_ERR_BAD_FD));
    w.router.instance_gone(gone);
    let new = w.open(P1, FILE_A).unwrap();
    assert_ne!(new, old);
    assert_eq!(
        w.io.backends[&CAP_A2]
            .open
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [HANDLE_BASE]
    );
    assert_eq!(w.read(P1, old, 4), Err(VFS_STATUS_ERR_BAD_FD));
    assert_eq!(w.read(P1, new, 4).unwrap(), bytes_b()[..4]);
    assert_eq!(
        w.calls(VFS_OP_READ),
        reads + 1,
        "the old fd was never forwarded"
    );
    let closes = w.calls(VFS_OP_CLOSE);
    w.close(P1, old).unwrap();
    assert_eq!(
        w.calls(VFS_OP_CLOSE),
        closes,
        "closing a stale fd owes the new backend nothing"
    );
    assert_eq!(w.io.backends[&CAP_A2].open.len(), 1);
}

// ── F14: canonical paths ──────────────────────────────────────────────────────────────────────

#[test]
fn vc1c_open_and_statx_forward_the_canonical_path_they_routed_on() {
    let mut w = World::new();
    for spelling in [
        &b"/a//f"[..],
        b"/a/./f",
        b"/b/../a/f",
        b"//a/f/",
        b"/../a/f",
        b"/a/f/.",
    ] {
        let fd = w.open_with(P1, 0, spelling, 0o2, 0o644).unwrap();
        assert_eq!(
            w.last_path(VFS_OP_OPENAT),
            FILE_A,
            "{:?}",
            core::str::from_utf8(spelling)
        );
        let last = w.io.calls.last().unwrap();
        assert_eq!(last.cap, CAP_A);
        let a = OpenAtInlinePath::decode(&last.payload).unwrap();
        assert_eq!((a.dirfd, a.flags, a.mode), (0, 0o2, 0o644));
        w.close(P1, fd).unwrap();
        assert_eq!(w.statx(P1, spelling), Ok(200));
        assert_eq!(w.last_path(VFS_OP_STATX), FILE_A);
        let s = StatxInlinePath::decode(&w.io.calls.last().unwrap().payload).unwrap();
        assert_eq!((s.dirfd, s.flags, s.mask_or_buf), (0, 0x100, 0x7ff));
    }
}

#[test]
fn vc1c_dirfd_is_not_forwarded() {
    let mut w = World::new();
    let fa = w.open(P1, FILE_A).unwrap();
    let f2 = w.open_with(P1, fa, b"/b/f", 0, 0).unwrap();
    let a = OpenAtInlinePath::decode(&w.io.calls.last().unwrap().payload).unwrap();
    assert_eq!((a.dirfd, a.path), (0, FILE_B));
    assert_eq!(w.read(P1, f2, 2).unwrap(), bytes_b()[..2]);
}

#[test]
fn vc1c_mount_component_boundaries() {
    let mut w = World::new();
    assert!(w.mounts.register(b"/a/b/", "ab", CAP_B));
    // "/a/bx" is not under "/a/b/"; "/a/b" and "/a/b/x" are.
    assert_eq!(w.statx(P1, b"/a/bx"), Ok(1));
    assert_eq!(w.io.calls.last().unwrap().cap, CAP_A);
    assert_eq!(w.statx(P1, b"/a/b/x"), Ok(1));
    assert_eq!(w.io.calls.last().unwrap().cap, CAP_B);
    assert_eq!(w.statx(P1, b"/a/b/"), Ok(0));
    assert_eq!(
        (w.io.calls.last().unwrap().cap, w.last_path(VFS_OP_STATX)),
        (CAP_B, b"/a/b".to_vec())
    );
    assert_eq!(w.statx(P1, b"/a/b/./../bx"), Ok(1));
    assert_eq!(w.io.calls.last().unwrap().cap, CAP_A);
    let n = w.io.calls.len();
    assert_eq!(w.statx(P1, b"/ab/f"), Err(VFS_STATUS_ERR_NO_MOUNT));
    assert_eq!(w.statx(P1, b"/"), Err(VFS_STATUS_ERR_NO_MOUNT));
    assert_eq!(w.statx(P1, b"/a/.."), Err(VFS_STATUS_ERR_NO_MOUNT));
    assert_eq!(w.statx(P1, b"a/f"), Err(VFS_STATUS_ERR_INVALID_PATH));
    assert_eq!(w.io.calls.len(), n);
}

#[test]
fn vc1c_wire_limit_is_respected() {
    let mut w = World::new();
    // 96 bytes on the wire that normalize to a short path are fine; the canonical form is never
    // longer than what was decoded.
    let mut long = b"/a".to_vec();
    while long.len() < 94 {
        long.extend_from_slice(b"/.");
    }
    long.extend_from_slice(b"/f");
    assert!(long.len() <= 96);
    assert_eq!(w.statx(P1, &long), Ok(200));
    assert_eq!(w.last_path(VFS_OP_STATX), FILE_A);
}
