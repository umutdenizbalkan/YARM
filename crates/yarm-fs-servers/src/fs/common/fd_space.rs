// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-CORRECT1C — the resident VFS's client file-descriptor space.
//!
//! A **descriptor** is what a client holds: a number chosen by VFS, unique within its owning
//! **session**. A session is a process, named by the kernel-authenticated
//! `(thread_group_id, process_incarnation)` pair (NR 21 `IpcCallerSession`): every thread of the
//! process shares it, and it is never reused, unlike a TID. A descriptor refers to an **open file
//! description** (OFD): the exact backend instance (`BackendRef`: the mount instance VFS assigned,
//! and VFS's own full-width endpoint capability for it) and the backend's handle. The OFD is what
//! owns the backend handle's lifetime; the client never sees a backend handle, and VFS never
//! forwards a client fd.
//!
//! **Open** is reserve → backend OPENAT → commit or abort, so a full table refuses before anything
//! is opened at a backend. **Close** removes the descriptor at once; when the OFD has no more
//! descriptors the caller must close the backend handle and report how that went:
//!
//! - `Closed` / `Refused` (the backend answered): the OFD is gone;
//! - `NotDelivered` (the NR 6 failed, so nothing reached the backend): the handle is still open
//!   there and still ours — it goes to the pending list and is retried against the SAME instance;
//! - `Ambiguous` (the request may have been delivered but no valid answer came back): it may or
//!   may not still be open. It is **quarantined**: never retried, because the backend may since
//!   have given the same number to someone else. It is counted and reported.
//!
//! A session that exits is reclaimed whole (`reclaim_session`): its descriptors go, and each OFD
//! left without descriptors is closed as above. A backend instance that goes away
//! (`invalidate_instance`) leaves its descriptors STALE: they answer bad-fd and are never
//! forwarded — so a later backend that reuses the old handle numbers can never be addressed
//! through them — and pending closes for it are dropped (the instance holds nothing any more).
//!
//! No `dup`: one descriptor per OFD (the reference count is kept so the model is ready for it).

/// Descriptors and OFDs VFS holds at once.
pub const MAX_DESCRIPTORS: usize = 32;
/// Descriptors one session may hold: no client can take the whole table.
pub const MAX_FDS_PER_SESSION: usize = 16;
/// Backend closes that could not be delivered and wait for a retry, plus quarantined ones.
pub const MAX_PENDING_CLOSES: usize = 8;
/// The first client fd (0, 1 and 2 are left to the client).
pub const FIRST_CLIENT_FD: u64 = 3;

/// A process, as the kernel names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId {
    pub thread_group_id: u64,
    pub incarnation: u64,
}

/// One backend instance: VFS's mount instance id and its own endpoint capability for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendRef {
    pub instance: u64,
    pub cap: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OfdState {
    /// Reserved for an OPENAT in flight: no handle yet, not resolvable.
    Reserved,
    Open,
    /// Its backend instance is gone: never forwarded.
    Stale,
    /// The last descriptor closed; the backend close is in flight.
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ofd {
    backend: BackendRef,
    handle: u64,
    refs: u8,
    state: OfdState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Descriptor {
    session: SessionId,
    fd: u64,
    ofd: u8,
}

/// The token for an OPENAT in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    pub session: SessionId,
    pub fd: u64,
    desc: u8,
    ofd: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveError {
    /// The session already holds `MAX_FDS_PER_SESSION` descriptors.
    SessionFull,
    /// VFS's table is full.
    TableFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// No such descriptor in this session.
    BadFd,
    /// The descriptor exists but its backend instance is gone.
    Stale,
}

/// What a resolved descriptor forwards to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub backend: BackendRef,
    pub handle: u64,
}

/// A backend close the caller must perform, then settle with [`FdSpace::close_settled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendClose {
    pub backend: BackendRef,
    pub handle: u64,
    token: CloseToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseToken {
    Ofd(u8),
    Pending(u8),
}

/// How a backend close went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    Closed,
    Refused(u32),
    NotDelivered,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Retry,
    Quarantined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    backend: BackendRef,
    handle: u64,
    kind: PendingKind,
    in_flight: bool,
}

/// Table occupancy, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub descriptors: usize,
    pub ofds: usize,
    pub sessions: usize,
    pub pending_retry: usize,
    pub quarantined: usize,
    pub pending_overflow: u64,
}

#[derive(Debug, Clone)]
pub struct FdSpace {
    desc: [Option<Descriptor>; MAX_DESCRIPTORS],
    ofd: [Option<Ofd>; MAX_DESCRIPTORS],
    pending: [Option<Pending>; MAX_PENDING_CLOSES],
    pending_overflow: u64,
}

impl Default for FdSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl FdSpace {
    pub const fn new() -> Self {
        Self {
            desc: [None; MAX_DESCRIPTORS],
            ofd: [None; MAX_DESCRIPTORS],
            pending: [None; MAX_PENDING_CLOSES],
            pending_overflow: 0,
        }
    }

    fn session_fds(&self, session: SessionId) -> usize {
        self.desc
            .iter()
            .flatten()
            .filter(|d| d.session == session)
            .count()
    }

    fn find(&self, session: SessionId, fd: u64) -> Option<usize> {
        self.desc
            .iter()
            .position(|d| matches!(d, Some(d) if d.session == session && d.fd == fd))
    }

    /// Reserve a descriptor and an OFD for an OPENAT to `backend`, before anything is opened.
    pub fn reserve(
        &mut self,
        session: SessionId,
        backend: BackendRef,
    ) -> Result<Reservation, ReserveError> {
        if self.session_fds(session) >= MAX_FDS_PER_SESSION {
            return Err(ReserveError::SessionFull);
        }
        let desc = self.desc.iter().position(Option::is_none);
        let ofd = self.ofd.iter().position(Option::is_none);
        let (Some(desc), Some(ofd)) = (desc, ofd) else {
            return Err(ReserveError::TableFull);
        };
        // The lowest number this session does not hold.
        let mut fd = FIRST_CLIENT_FD;
        while self.find(session, fd).is_some() {
            fd += 1;
        }
        self.ofd[ofd] = Some(Ofd {
            backend,
            handle: 0,
            refs: 1,
            state: OfdState::Reserved,
        });
        self.desc[desc] = Some(Descriptor {
            session,
            fd,
            ofd: ofd as u8,
        });
        Ok(Reservation {
            session,
            fd,
            desc: desc as u8,
            ofd: ofd as u8,
        })
    }

    /// The backend opened `handle`: the descriptor becomes usable. Returns the client fd.
    pub fn commit(&mut self, r: Reservation, handle: u64) -> u64 {
        if let Some(o) = self.ofd[r.ofd as usize].as_mut() {
            o.handle = handle;
            o.state = OfdState::Open;
        }
        r.fd
    }

    /// The OPENAT did not open anything (or the open is being rolled back after a successful
    /// backend open — the caller then closes `handle` itself): release the reservation.
    pub fn abort(&mut self, r: Reservation) {
        self.desc[r.desc as usize] = None;
        self.ofd[r.ofd as usize] = None;
    }

    /// The OFD a descriptor names, if it may be forwarded.
    pub fn resolve(&self, session: SessionId, fd: u64) -> Result<Target, ResolveError> {
        let d = self
            .find(session, fd)
            .and_then(|i| self.desc[i])
            .ok_or(ResolveError::BadFd)?;
        let o = self.ofd[d.ofd as usize].ok_or(ResolveError::BadFd)?;
        match o.state {
            OfdState::Open => Ok(Target {
                backend: o.backend,
                handle: o.handle,
            }),
            OfdState::Stale => Err(ResolveError::Stale),
            OfdState::Reserved | OfdState::Closing => Err(ResolveError::BadFd),
        }
    }

    /// Drop one descriptor reference to OFD `idx`; the backend close it now needs, if any.
    fn release_ref(&mut self, idx: u8) -> Option<BackendClose> {
        let o = self.ofd[idx as usize].as_mut()?;
        o.refs = o.refs.saturating_sub(1);
        if o.refs > 0 {
            return None;
        }
        match o.state {
            OfdState::Open => {
                o.state = OfdState::Closing;
                Some(BackendClose {
                    backend: o.backend,
                    handle: o.handle,
                    token: CloseToken::Ofd(idx),
                })
            }
            // Nothing is open at a live backend for these.
            OfdState::Stale | OfdState::Reserved | OfdState::Closing => {
                self.ofd[idx as usize] = None;
                None
            }
        }
    }

    /// The client closed `fd`: the descriptor is gone whatever the backend says.
    pub fn close(
        &mut self,
        session: SessionId,
        fd: u64,
    ) -> Result<Option<BackendClose>, ResolveError> {
        let i = self.find(session, fd).ok_or(ResolveError::BadFd)?;
        let d = self.desc[i].take().ok_or(ResolveError::BadFd)?;
        Ok(self.release_ref(d.ofd))
    }

    /// Record how a backend close went.
    pub fn close_settled(&mut self, c: BackendClose, outcome: CloseOutcome) {
        match c.token {
            CloseToken::Ofd(idx) => {
                self.ofd[idx as usize] = None;
                match outcome {
                    CloseOutcome::Closed | CloseOutcome::Refused(_) => {}
                    CloseOutcome::NotDelivered => self.push_pending(c, PendingKind::Retry),
                    CloseOutcome::Ambiguous => self.push_pending(c, PendingKind::Quarantined),
                }
            }
            CloseToken::Pending(idx) => {
                let slot = &mut self.pending[idx as usize];
                match outcome {
                    CloseOutcome::Closed | CloseOutcome::Refused(_) => *slot = None,
                    CloseOutcome::NotDelivered => {
                        if let Some(p) = slot.as_mut() {
                            p.in_flight = false;
                        }
                    }
                    CloseOutcome::Ambiguous => {
                        if let Some(p) = slot.as_mut() {
                            p.in_flight = false;
                            p.kind = PendingKind::Quarantined;
                        }
                    }
                }
            }
        }
    }

    fn push_pending(&mut self, c: BackendClose, kind: PendingKind) {
        match self.pending.iter_mut().find(|p| p.is_none()) {
            Some(slot) => {
                *slot = Some(Pending {
                    backend: c.backend,
                    handle: c.handle,
                    kind,
                    in_flight: false,
                })
            }
            None => self.pending_overflow = self.pending_overflow.saturating_add(1),
        }
    }

    /// The next undelivered backend close to retry (same instance, same handle), if any.
    pub fn next_retry(&mut self) -> Option<BackendClose> {
        for (i, slot) in self.pending.iter_mut().enumerate() {
            if let Some(p) = slot.as_mut()
                && p.kind == PendingKind::Retry
                && !p.in_flight
            {
                p.in_flight = true;
                return Some(BackendClose {
                    backend: p.backend,
                    handle: p.handle,
                    token: CloseToken::Pending(i as u8),
                });
            }
        }
        None
    }

    /// Every distinct session that holds a descriptor (at most `MAX_DESCRIPTORS`).
    pub fn sessions(&self) -> ([Option<SessionId>; MAX_DESCRIPTORS], usize) {
        let mut out = [None; MAX_DESCRIPTORS];
        let mut n = 0;
        for d in self.desc.iter().flatten() {
            if !out[..n].contains(&Some(d.session)) {
                out[n] = Some(d.session);
                n += 1;
            }
        }
        (out, n)
    }

    /// The session has exited: remove every descriptor it held. Returns the backend closes now
    /// owed (each to be settled) and how many descriptors went.
    pub fn reclaim_session(
        &mut self,
        session: SessionId,
    ) -> ([Option<BackendClose>; MAX_FDS_PER_SESSION], usize) {
        let mut out = [None; MAX_FDS_PER_SESSION];
        let mut n = 0;
        let mut removed = 0;
        for i in 0..MAX_DESCRIPTORS {
            if let Some(d) = self.desc[i]
                && d.session == session
            {
                self.desc[i] = None;
                removed += 1;
                if let Some(c) = self.release_ref(d.ofd)
                    && n < out.len()
                {
                    out[n] = Some(c);
                    n += 1;
                }
            }
        }
        (out, removed)
    }

    /// Backend instance `instance` is gone: its descriptors go stale and its pending closes are
    /// dropped. Returns how many OFDs went stale.
    pub fn invalidate_instance(&mut self, instance: u64) -> usize {
        let mut n = 0;
        for o in self.ofd.iter_mut().flatten() {
            if o.backend.instance == instance && o.state == OfdState::Open {
                o.state = OfdState::Stale;
                n += 1;
            }
        }
        for slot in self.pending.iter_mut() {
            if matches!(slot, Some(p) if p.backend.instance == instance) {
                *slot = None;
            }
        }
        n
    }

    pub fn counts(&self) -> Counts {
        let (_, sessions) = self.sessions();
        Counts {
            descriptors: self.desc.iter().flatten().count(),
            ofds: self.ofd.iter().flatten().count(),
            sessions,
            pending_retry: self
                .pending
                .iter()
                .flatten()
                .filter(|p| p.kind == PendingKind::Retry)
                .count(),
            quarantined: self
                .pending
                .iter()
                .flatten()
                .filter(|p| p.kind == PendingKind::Quarantined)
                .count(),
            pending_overflow: self.pending_overflow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: SessionId = SessionId {
        thread_group_id: 1,
        incarnation: 11,
    };
    const B: SessionId = SessionId {
        thread_group_id: 2,
        incarnation: 22,
    };
    /// The same thread-group id as A, a later process: must not see A's files.
    const A_LATER: SessionId = SessionId {
        thread_group_id: 1,
        incarnation: 99,
    };
    /// Generation-witness-sized endpoint capabilities.
    const RAM: BackendRef = BackendRef {
        instance: 1,
        cap: (0x1_0001u64 << 16) | 12,
    };
    const RAM_B: BackendRef = BackendRef {
        instance: 2,
        cap: (0x1_0001u64 << 16) | 13,
    };

    fn open(s: &mut FdSpace, sess: SessionId, b: BackendRef, handle: u64) -> u64 {
        let r = s.reserve(sess, b).expect("reserve");
        s.commit(r, handle)
    }

    #[test]
    fn vc1c_same_backend_handle_on_two_backends_gives_two_fds_each_resolving_to_its_own() {
        let mut s = FdSpace::new();
        let fa = open(&mut s, A, RAM, 103);
        let fb = open(&mut s, A, RAM_B, 103);
        assert_ne!(fa, fb, "one client, two files: two descriptors");
        assert_ne!(fa, 103, "the client fd is not the backend handle");
        assert_eq!(
            s.resolve(A, fa),
            Ok(Target {
                backend: RAM,
                handle: 103
            })
        );
        assert_eq!(
            s.resolve(A, fb),
            Ok(Target {
                backend: RAM_B,
                handle: 103
            })
        );
        let c = s
            .close(A, fa)
            .unwrap()
            .expect("last descriptor: backend close owed");
        assert_eq!((c.backend, c.handle), (RAM, 103));
        s.close_settled(c, CloseOutcome::Closed);
        assert_eq!(s.resolve(A, fa), Err(ResolveError::BadFd));
        assert_eq!(
            s.resolve(A, fb),
            Ok(Target {
                backend: RAM_B,
                handle: 103
            })
        );
        assert_eq!(s.counts().descriptors, 1);
    }

    #[test]
    fn vc1c_sessions_have_independent_numbers_and_never_resolve_each_others_files() {
        let mut s = FdSpace::new();
        let fa = open(&mut s, A, RAM, 7);
        let fb = open(&mut s, B, RAM_B, 9);
        assert_eq!(fa, fb, "separate sessions may hold the same number");
        assert_eq!(s.resolve(A, fa).unwrap().backend, RAM);
        assert_eq!(s.resolve(B, fb).unwrap().backend, RAM_B);
        assert_eq!(
            s.resolve(A_LATER, fa),
            Err(ResolveError::BadFd),
            "a reused tgid is another session"
        );
        assert_eq!(s.close(B, fa).map(|c| c.is_some()), Ok(true));
        assert!(s.resolve(A, fa).is_ok(), "B's close did not touch A");
    }

    #[test]
    fn vc1c_lowest_free_number_and_per_session_limit_refuse_before_any_backend_open() {
        let mut s = FdSpace::new();
        for i in 0..MAX_FDS_PER_SESSION {
            assert_eq!(
                open(&mut s, A, RAM, 100 + i as u64),
                FIRST_CLIENT_FD + i as u64
            );
        }
        assert_eq!(s.reserve(A, RAM), Err(ReserveError::SessionFull));
        let c = s.close(A, FIRST_CLIENT_FD + 2).unwrap().unwrap();
        s.close_settled(c, CloseOutcome::Closed);
        assert_eq!(
            open(&mut s, A, RAM, 200),
            FIRST_CLIENT_FD + 2,
            "lowest free number"
        );
        // Another session can still open: no single client takes the whole table.
        assert!(s.reserve(B, RAM).is_ok());
    }

    #[test]
    fn vc1c_a_full_table_refuses_and_an_aborted_reservation_leaves_nothing() {
        let mut s = FdSpace::new();
        let mut n = 0;
        'fill: for t in 1..=8u64 {
            let sess = SessionId {
                thread_group_id: t,
                incarnation: t,
            };
            for _ in 0..MAX_FDS_PER_SESSION {
                if n == MAX_DESCRIPTORS {
                    break 'fill;
                }
                open(&mut s, sess, RAM, 1000 + n as u64);
                n += 1;
            }
        }
        let before = s.counts();
        assert_eq!(before.descriptors, MAX_DESCRIPTORS);
        let late = SessionId {
            thread_group_id: 77,
            incarnation: 77,
        };
        assert_eq!(s.reserve(late, RAM), Err(ReserveError::TableFull));
        assert_eq!(s.counts(), before, "a refused reservation changes nothing");
        let mut s2 = FdSpace::new();
        let r = s2.reserve(A, RAM).unwrap();
        s2.abort(r);
        assert_eq!(
            s2.counts(),
            Counts::default(),
            "an aborted reservation leaves nothing"
        );
    }

    #[test]
    fn vc1c_a_reserved_descriptor_is_not_resolvable_until_committed() {
        let mut s = FdSpace::new();
        let r = s.reserve(A, RAM).unwrap();
        assert_eq!(s.resolve(A, r.fd), Err(ResolveError::BadFd));
        s.commit(r, 5);
        assert!(s.resolve(A, r.fd).is_ok());
    }

    #[test]
    fn vc1c_undelivered_close_is_retried_against_the_same_instance_and_handle() {
        let mut s = FdSpace::new();
        let f = open(&mut s, A, RAM, 42);
        let c = s.close(A, f).unwrap().unwrap();
        s.close_settled(c, CloseOutcome::NotDelivered);
        assert_eq!(s.counts().pending_retry, 1);
        let r = s.next_retry().expect("retry");
        assert_eq!((r.backend, r.handle), (RAM, 42));
        assert!(
            s.next_retry().is_none(),
            "one retry in flight at a time per entry"
        );
        s.close_settled(r, CloseOutcome::Closed);
        assert_eq!(s.counts(), Counts::default());
    }

    #[test]
    fn vc1c_ambiguous_close_is_quarantined_and_never_retried() {
        let mut s = FdSpace::new();
        let f = open(&mut s, A, RAM, 42);
        let c = s.close(A, f).unwrap().unwrap();
        s.close_settled(c, CloseOutcome::Ambiguous);
        assert_eq!(s.counts().quarantined, 1);
        assert!(
            s.next_retry().is_none(),
            "an ambiguous close could hit a recycled handle"
        );
        // A retry that turns ambiguous is quarantined too.
        let f = open(&mut s, A, RAM, 43);
        let c = s.close(A, f).unwrap().unwrap();
        s.close_settled(c, CloseOutcome::NotDelivered);
        let r = s.next_retry().unwrap();
        s.close_settled(r, CloseOutcome::Ambiguous);
        assert_eq!(s.counts().quarantined, 2);
        assert!(s.next_retry().is_none());
    }

    #[test]
    fn vc1c_session_exit_reclaims_every_descriptor_and_owes_each_backend_close_once() {
        let mut s = FdSpace::new();
        let keep = open(&mut s, A, RAM, 1);
        open(&mut s, B, RAM, 2);
        open(&mut s, B, RAM_B, 3);
        let (closes, removed) = s.reclaim_session(B);
        assert_eq!(removed, 2);
        let got: alloc::vec::Vec<_> = closes
            .iter()
            .flatten()
            .map(|c| (c.backend, c.handle))
            .collect();
        assert_eq!(got, [(RAM, 2), (RAM_B, 3)]);
        for c in closes.iter().flatten() {
            s.close_settled(*c, CloseOutcome::Closed);
        }
        let k = s.counts();
        assert_eq!((k.descriptors, k.ofds, k.sessions), (1, 1, 1));
        assert!(s.resolve(A, keep).is_ok(), "another session is untouched");
        assert_eq!(s.reclaim_session(B).1, 0, "reclaiming twice is a no-op");
    }

    #[test]
    fn vc1c_backend_restart_leaves_old_descriptors_stale_and_never_forwarded() {
        let mut s = FdSpace::new();
        let f = open(&mut s, A, RAM, 100);
        let f2 = open(&mut s, A, RAM, 101);
        let c = s.close(A, f2).unwrap().unwrap();
        s.close_settled(c, CloseOutcome::NotDelivered);
        assert_eq!(s.invalidate_instance(RAM.instance), 1);
        assert_eq!(s.resolve(A, f), Err(ResolveError::Stale), "never forwarded");
        assert!(
            s.next_retry().is_none(),
            "pending closes for a gone instance are dropped"
        );
        // The restarted backend is a new instance that reuses handle 100.
        let new = BackendRef {
            instance: 9,
            cap: RAM.cap + 1,
        };
        let g = open(&mut s, A, new, 100);
        assert_ne!(f, g);
        assert_eq!(s.resolve(A, g).unwrap().backend, new);
        assert_eq!(s.resolve(A, f), Err(ResolveError::Stale));
        // Closing a stale descriptor owes the gone instance nothing.
        assert_eq!(s.close(A, f), Ok(None));
    }

    #[test]
    fn vc1c_pending_list_overflow_is_counted() {
        let mut s = FdSpace::new();
        for i in 0..(MAX_PENDING_CLOSES + 2) {
            let f = open(&mut s, A, RAM, i as u64);
            let c = s.close(A, f).unwrap().unwrap();
            s.close_settled(c, CloseOutcome::Ambiguous);
        }
        let k = s.counts();
        assert_eq!(k.quarantined, MAX_PENDING_CLOSES);
        assert_eq!(k.pending_overflow, 2);
        assert_eq!(k.ofds, 0);
    }
}
