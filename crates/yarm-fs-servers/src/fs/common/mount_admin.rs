// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! VFS-CORRECT1B — authorized mount registration (the resident VFS's mount-admin state machine).
//!
//! Mount administration is a capability, not a number. The administrator holds the SEND side of
//! the VFS **mount-admin endpoint** (provisioned for VFS at spawn; only its receive side is in
//! VFS's CNode). A registration is two steps:
//!
//! 1. **stage** — NR 1 to the admin endpoint, `VFS_OP_MOUNT_STAGE`, carrying the backend's
//!    service endpoint as the message's TRANSFERRED capability. The kernel validates the
//!    administrator's capability (stale or missing → the send is refused at the sender, nothing
//!    is queued) and installs the backend endpoint in VFS's CNode under a VFS-local identity.
//! 2. **commit** — NR 6 to the PUBLIC VFS endpoint, `VFS_OP_MOUNT_COMMIT`, naming the prefix. VFS
//!    drains its admin endpoint (only messages that arrived there count as authorized) and
//!    matches the commit to a staged mount by the kernel-authenticated sender TID and the prefix.
//!
//! A message that reached the admin endpoint was sent by a holder of its SEND capability, so the
//! commit carries authority without VFS trusting anything in a payload; an ordinary client — one
//! holding only the public endpoint — can stage nothing, and its commit is refused.
//!
//! Before a mount is published: flags (only `VFS_MOUNT_FLAGS_SUPPORTED`), prefix validity,
//! duplicate, table capacity, then the **registration handshake**: one `VFS_OP_STATX` of the
//! prefix through the staged capability. The kernel owns the object-kind, SEND-right and
//! generation checks and answers that call with `WrongObject`, `MissingRight` or
//! `InvalidCapability` before anything is delivered; a backend that answers with a valid
//! completion is live and speaks the completion protocol. Only then is the mount inserted.
//!
//! A capability transfer DELEGATES: the kernel installs a copy in VFS's CNode and the sender keeps
//! its own (observed live: the identity an administrator staged stays usable at the sender), and
//! no system call deletes a capability. So VFS cannot give back what a refused attempt brought
//! in; it ACCOUNTS for it instead. A refused commit leaves the routing table exactly as it was and
//! moves the staged capability into the bounded `retained` record (logged, never routed). A stage
//! that cannot be recorded (malformed payload, duplicate stage) is retained the same way. VFS
//! drains its admin endpoint only while every staged mount could still be retained, so a refusal
//! always has a place in the record; past that, further admin messages wait in the kernel's
//! queue and commits are answered `NOT_STAGED`.

use yarm_ipc_abi::vfs_abi::{
    MountStageArgs, VFS_MOUNT_FLAGS_SUPPORTED, VFS_MOUNT_REGISTER_PREFIX_MAX,
    VFS_MOUNT_STATUS_ERR_BACKEND, VFS_MOUNT_STATUS_ERR_DUPLICATE, VFS_MOUNT_STATUS_ERR_FULL,
    VFS_MOUNT_STATUS_ERR_INVALID_PREFIX, VFS_MOUNT_STATUS_ERR_NOT_STAGED,
    VFS_MOUNT_STATUS_ERR_UNSUPPORTED_FLAGS, VFS_MOUNT_STATUS_OK, VFS_OP_MOUNT_STAGE,
};

/// Staged mounts VFS holds at once. Admin messages beyond this stay queued in the kernel (VFS
/// drains only while a slot is free), so a full table never loses a capability.
pub const MAX_STAGED_MOUNTS: usize = 4;
/// Capabilities kept from refused or unrecordable stages (VFS's CNode cannot release them).
pub const MAX_RETAINED_CAPS: usize = 8;

/// Why the mount table refused a prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountTableRefusal {
    Duplicate,
    Full,
    InvalidPrefix,
}

/// The routing table, as the state machine needs it.
pub trait MountTableOps {
    /// Would `insert` accept this prefix now? Called before the handshake.
    fn precheck(&self, prefix: &[u8]) -> Result<(), MountTableRefusal>;
    /// Publish the mount: `backend_cap` is VFS-local.
    fn insert(
        &mut self,
        prefix: &[u8],
        backend_cap: u64,
        flags: u64,
    ) -> Result<(), MountTableRefusal>;
}

/// How the registration handshake failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeFault {
    /// NR 6 through the staged capability was refused by the kernel; the code is the
    /// `SyscallError` value (`InvalidCapability` 3, `MissingRight` 4, `WrongObject` 5, ...).
    Refused(usize),
    /// The call was delivered but no valid completion came back.
    NoValidCompletion,
}

/// The registration handshake: one `VFS_OP_STATX` of `prefix` through `backend_cap`.
pub trait BackendHandshake {
    fn handshake(&mut self, backend_cap: u64, prefix: &[u8]) -> Result<(), HandshakeFault>;
}

/// One staged mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedMount {
    pub sender_tid: u64,
    pub flags: u64,
    /// The administrator's own slot for the backend endpoint, as it reported it (logged only).
    pub sender_local_cap: u64,
    /// The backend endpoint's VFS-local identity (the received transferred capability).
    pub backend_cap: u64,
    prefix: [u8; VFS_MOUNT_REGISTER_PREFIX_MAX],
    prefix_len: u8,
}

impl StagedMount {
    pub fn prefix(&self) -> &[u8] {
        &self.prefix[..self.prefix_len as usize]
    }
}

/// What happened to one message drained from the admin endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    /// Recorded; commit it to publish.
    Staged(StagedMount),
    /// Not recorded (malformed payload or wrong opcode, or the same sender already staged this
    /// prefix); its capability is in the retained record.
    Retained {
        backend_cap: u64,
        reason: &'static str,
    },
    /// Not recorded and carried no capability.
    Ignored { reason: &'static str },
}

/// The result of a commit, for the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitOutcome {
    /// `VFS_MOUNT_STATUS_*`.
    pub status: u32,
    /// On a refusal of a staged mount: its capability, now in the retained record.
    pub retained_cap: Option<u64>,
    /// On success: the VFS-local capability now routed for the prefix.
    pub published_cap: Option<u64>,
    /// On a handshake failure: why.
    pub handshake_fault: Option<HandshakeFault>,
    /// The staged mount the commit consumed, if any.
    pub staged: Option<StagedMount>,
}

/// The mount-admin state: staged mounts and retained capabilities.
#[derive(Debug, Clone, Default)]
pub struct MountAdmin {
    staged: [Option<StagedMount>; MAX_STAGED_MOUNTS],
    retained: [u64; MAX_RETAINED_CAPS],
    retained_len: usize,
    /// Retained capabilities beyond `MAX_RETAINED_CAPS` (still in VFS's CNode, counted).
    retained_overflow: u64,
}

impl MountAdmin {
    pub const fn new() -> Self {
        Self {
            staged: [None; MAX_STAGED_MOUNTS],
            retained: [0; MAX_RETAINED_CAPS],
            retained_len: 0,
            retained_overflow: 0,
        }
    }

    /// Whether VFS may drain one more message from its admin endpoint: a staged slot is free
    /// AND every staged mount (this one included) could still be retained if refused.
    pub fn has_room(&self) -> bool {
        self.staged.iter().any(Option::is_none)
            && self.retained_len + self.staged_count() < MAX_RETAINED_CAPS
    }

    fn refused(&mut self, status: u32, staged: Option<StagedMount>) -> CommitOutcome {
        if let Some(s) = staged {
            self.retain(s.backend_cap);
        }
        CommitOutcome {
            status,
            retained_cap: staged.map(|s| s.backend_cap),
            published_cap: None,
            handshake_fault: None,
            staged,
        }
    }

    pub fn staged_count(&self) -> usize {
        self.staged.iter().filter(|s| s.is_some()).count()
    }

    /// Capabilities kept from unrecordable stages, plus how many overflowed the record.
    pub fn retained(&self) -> (&[u64], u64) {
        (&self.retained[..self.retained_len], self.retained_overflow)
    }

    fn retain(&mut self, cap: u64) {
        if self.retained_len < MAX_RETAINED_CAPS {
            self.retained[self.retained_len] = cap;
            self.retained_len += 1;
        } else {
            self.retained_overflow = self.retained_overflow.saturating_add(1);
        }
    }

    /// Record one message that arrived on the ADMIN endpoint (its arrival there is the
    /// administrator's authority). Call only while [`has_room`](Self::has_room).
    pub fn stage(
        &mut self,
        sender_tid: u64,
        opcode: u16,
        transferred_cap: Option<u64>,
        payload: &[u8],
    ) -> StageOutcome {
        let not_recorded = |admin: &mut Self, reason: &'static str| match transferred_cap {
            Some(cap) => {
                admin.retain(cap);
                StageOutcome::Retained {
                    backend_cap: cap,
                    reason,
                }
            }
            None => StageOutcome::Ignored { reason },
        };
        if opcode != VFS_OP_MOUNT_STAGE {
            return not_recorded(self, "opcode");
        }
        let Some(backend_cap) = transferred_cap else {
            return StageOutcome::Ignored { reason: "no_cap" };
        };
        let Some(args) = MountStageArgs::decode(payload) else {
            return not_recorded(self, "payload");
        };
        if self
            .staged
            .iter()
            .flatten()
            .any(|s| s.sender_tid == sender_tid && s.prefix() == args.prefix)
        {
            return not_recorded(self, "already_staged");
        }
        let Some(slot) = self.staged.iter_mut().find(|s| s.is_none()) else {
            // Unreachable when the caller drains only while `has_room`; never drop the cap.
            return not_recorded(self, "full");
        };
        let mut prefix = [0u8; VFS_MOUNT_REGISTER_PREFIX_MAX];
        prefix[..args.prefix.len()].copy_from_slice(args.prefix);
        let staged = StagedMount {
            sender_tid,
            flags: args.flags,
            sender_local_cap: args.sender_local_cap,
            backend_cap,
            prefix,
            prefix_len: args.prefix.len() as u8,
        };
        *slot = Some(staged);
        StageOutcome::Staged(staged)
    }

    /// Commit `prefix` for `sender_tid` (both from the NR 6 request on the PUBLIC endpoint).
    pub fn commit(
        &mut self,
        sender_tid: u64,
        prefix: &[u8],
        table: &mut impl MountTableOps,
        backend: &mut impl BackendHandshake,
    ) -> CommitOutcome {
        let Some(slot) = self
            .staged
            .iter_mut()
            .find(|s| matches!(s, Some(s) if s.sender_tid == sender_tid && s.prefix() == prefix))
        else {
            return self.refused(VFS_MOUNT_STATUS_ERR_NOT_STAGED, None);
        };
        let staged = slot.take().expect("matched");
        if staged.flags & !VFS_MOUNT_FLAGS_SUPPORTED != 0 {
            return self.refused(VFS_MOUNT_STATUS_ERR_UNSUPPORTED_FLAGS, Some(staged));
        }
        if let Err(refusal) = table.precheck(staged.prefix()) {
            return self.refused(status_for(refusal), Some(staged));
        }
        if let Err(fault) = backend.handshake(staged.backend_cap, staged.prefix()) {
            let mut out = self.refused(VFS_MOUNT_STATUS_ERR_BACKEND, Some(staged));
            out.handshake_fault = Some(fault);
            return out;
        }
        if let Err(refusal) = table.insert(staged.prefix(), staged.backend_cap, staged.flags) {
            return self.refused(status_for(refusal), Some(staged));
        }
        CommitOutcome {
            status: VFS_MOUNT_STATUS_OK,
            retained_cap: None,
            published_cap: Some(staged.backend_cap),
            handshake_fault: None,
            staged: Some(staged),
        }
    }
}

pub const fn status_for(refusal: MountTableRefusal) -> u32 {
    match refusal {
        MountTableRefusal::Duplicate => VFS_MOUNT_STATUS_ERR_DUPLICATE,
        MountTableRefusal::Full => VFS_MOUNT_STATUS_ERR_FULL,
        MountTableRefusal::InvalidPrefix => VFS_MOUNT_STATUS_ERR_INVALID_PREFIX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use yarm_ipc_abi::vfs_abi::{
        MountRegisterArgs, VFS_MOUNT_FLAG_READONLY, VFS_MOUNT_STATUS_ERR_PERMISSION,
        VFS_OP_MOUNT_REGISTER,
    };

    /// A routing table with a fixed capacity and exact-prefix duplicates.
    struct Table {
        entries: Vec<(Vec<u8>, u64, u64)>,
        cap: usize,
    }
    impl MountTableOps for Table {
        fn precheck(&self, prefix: &[u8]) -> Result<(), MountTableRefusal> {
            if prefix.first() != Some(&b'/') {
                return Err(MountTableRefusal::InvalidPrefix);
            }
            if self.entries.iter().any(|(p, _, _)| p.as_slice() == prefix) {
                return Err(MountTableRefusal::Duplicate);
            }
            if self.entries.len() >= self.cap {
                return Err(MountTableRefusal::Full);
            }
            Ok(())
        }
        fn insert(&mut self, p: &[u8], cap: u64, flags: u64) -> Result<(), MountTableRefusal> {
            self.precheck(p)?;
            self.entries.push((p.to_vec(), cap, flags));
            Ok(())
        }
    }
    fn table(cap: usize) -> Table {
        Table {
            entries: alloc::vec![(b"/initramfs".to_vec(), 7, 1)],
            cap,
        }
    }

    /// The kernel's answer to the handshake call, scripted; records every call.
    struct Kernel {
        answer: Result<(), HandshakeFault>,
        calls: Vec<u64>,
    }
    impl BackendHandshake for Kernel {
        fn handshake(&mut self, cap: u64, _p: &[u8]) -> Result<(), HandshakeFault> {
            self.calls.push(cap);
            self.answer
        }
    }
    fn kernel(answer: Result<(), HandshakeFault>) -> Kernel {
        Kernel {
            answer,
            calls: Vec::new(),
        }
    }

    fn stage_payload(flags: u64, sender_local: u64, prefix: &[u8]) -> Vec<u8> {
        let (b, n) = MountStageArgs {
            flags,
            sender_local_cap: sender_local,
            prefix,
        }
        .encode()
        .expect("encode");
        b[..n].to_vec()
    }

    /// Generation 65,537: an ID above u32::MAX, as the cap-gen witness issues them.
    const BIG: u64 = (0x1_0001u64 << 16) | 5;

    #[test]
    fn vc1b_stage_args_roundtrip_and_reject_extra_bytes() {
        let p = stage_payload(VFS_MOUNT_FLAG_READONLY, BIG, b"/ram");
        let a = MountStageArgs::decode(&p).expect("decode");
        assert_eq!(a.flags, 1);
        assert_eq!(a.sender_local_cap, BIG);
        assert_eq!(a.prefix, b"/ram");
        let mut longer = p.clone();
        longer.push(0);
        assert!(MountStageArgs::decode(&longer).is_none());
        assert!(MountStageArgs::decode(&p[..p.len() - 1]).is_none());
    }

    #[test]
    fn vc1b_staged_then_committed_mount_publishes_the_vfs_local_cap() {
        let mut admin = MountAdmin::new();
        let s = admin.stage(
            1,
            VFS_OP_MOUNT_STAGE,
            Some(BIG),
            &stage_payload(0, 65551, b"/ram"),
        );
        assert!(
            matches!(s, StageOutcome::Staged(m) if m.backend_cap == BIG && m.sender_local_cap == 65551)
        );
        let mut t = table(4);
        let mut k = kernel(Ok(()));
        let out = admin.commit(1, b"/ram", &mut t, &mut k);
        assert_eq!(out.status, VFS_MOUNT_STATUS_OK);
        assert_eq!(out.published_cap, Some(BIG));
        assert_eq!(out.retained_cap, None);
        // The routed capability is the received one, full width — never the sender's slot.
        assert_eq!(t.entries.last().map(|e| e.1), Some(BIG));
        assert_eq!(k.calls, [BIG]);
        assert_eq!(admin.staged_count(), 0);
    }

    #[test]
    fn vc1b_commit_without_stage_is_refused_and_touches_nothing() {
        let mut admin = MountAdmin::new();
        let mut t = table(4);
        let mut k = kernel(Ok(()));
        let out = admin.commit(1, b"/ram", &mut t, &mut k);
        assert_eq!(out.status, VFS_MOUNT_STATUS_ERR_NOT_STAGED);
        assert_eq!(out.retained_cap, None);
        assert_eq!(t.entries.len(), 1);
        assert!(k.calls.is_empty());
    }

    #[test]
    fn vc1b_commit_by_another_sender_does_not_consume_the_stage() {
        let mut admin = MountAdmin::new();
        admin.stage(
            1,
            VFS_OP_MOUNT_STAGE,
            Some(BIG),
            &stage_payload(0, 9, b"/ram"),
        );
        let mut t = table(4);
        let mut k = kernel(Ok(()));
        // An ordinary client (tid 10009) commits the administrator's prefix: refused.
        assert_eq!(
            admin.commit(10009, b"/ram", &mut t, &mut k).status,
            VFS_MOUNT_STATUS_ERR_NOT_STAGED
        );
        assert_eq!(admin.staged_count(), 1);
        assert_eq!(
            admin.commit(1, b"/ram", &mut t, &mut k).status,
            VFS_MOUNT_STATUS_OK
        );
    }

    #[test]
    fn vc1b_unsupported_flags_refused_before_handshake_and_cap_returned() {
        let mut admin = MountAdmin::new();
        admin.stage(
            1,
            VFS_OP_MOUNT_STAGE,
            Some(BIG),
            &stage_payload(0x8, 9, b"/ram"),
        );
        let mut t = table(4);
        let mut k = kernel(Ok(()));
        let out = admin.commit(1, b"/ram", &mut t, &mut k);
        assert_eq!(out.status, VFS_MOUNT_STATUS_ERR_UNSUPPORTED_FLAGS);
        assert_eq!(out.retained_cap, Some(BIG));
        assert!(k.calls.is_empty());
        assert_eq!(t.entries.len(), 1);
    }

    #[test]
    fn vc1b_duplicate_and_capacity_refused_before_handshake_and_cap_returned() {
        let mut admin = MountAdmin::new();
        let mut k = kernel(Ok(()));
        admin.stage(
            1,
            VFS_OP_MOUNT_STAGE,
            Some(BIG),
            &stage_payload(0, 9, b"/initramfs"),
        );
        let mut t = table(4);
        let out = admin.commit(1, b"/initramfs", &mut t, &mut k);
        assert_eq!(out.status, VFS_MOUNT_STATUS_ERR_DUPLICATE);
        assert_eq!(out.retained_cap, Some(BIG));
        admin.stage(
            1,
            VFS_OP_MOUNT_STAGE,
            Some(BIG + 1),
            &stage_payload(0, 9, b"/ram"),
        );
        let mut full = table(1);
        let out = admin.commit(1, b"/ram", &mut full, &mut k);
        assert_eq!(out.status, VFS_MOUNT_STATUS_ERR_FULL);
        assert_eq!(out.retained_cap, Some(BIG + 1));
        assert!(k.calls.is_empty());
        assert_eq!(full.entries.len(), 1);
    }

    /// Wrong object kind, missing SEND right and a stale generation are the KERNEL's checks: the
    /// handshake's NR 6 is refused with WrongObject (5), MissingRight (4) or InvalidCapability
    /// (3). VFS refuses the commit, publishes nothing, and returns the capability.
    #[test]
    fn vc1b_kernel_refused_handshake_refuses_commit_for_each_kernel_check() {
        for code in [5usize, 4, 3] {
            let mut admin = MountAdmin::new();
            admin.stage(
                1,
                VFS_OP_MOUNT_STAGE,
                Some(BIG),
                &stage_payload(0, 9, b"/ram"),
            );
            let mut t = table(4);
            let mut k = kernel(Err(HandshakeFault::Refused(code)));
            let out = admin.commit(1, b"/ram", &mut t, &mut k);
            assert_eq!(out.status, VFS_MOUNT_STATUS_ERR_BACKEND, "code {code}");
            assert_eq!(out.handshake_fault, Some(HandshakeFault::Refused(code)));
            assert_eq!(out.retained_cap, Some(BIG));
            assert_eq!(t.entries.len(), 1);
        }
        let mut admin = MountAdmin::new();
        admin.stage(
            1,
            VFS_OP_MOUNT_STAGE,
            Some(BIG),
            &stage_payload(0, 9, b"/ram"),
        );
        let mut t = table(4);
        let mut k = kernel(Err(HandshakeFault::NoValidCompletion));
        assert_eq!(
            admin.commit(1, b"/ram", &mut t, &mut k).status,
            VFS_MOUNT_STATUS_ERR_BACKEND
        );
    }

    #[test]
    fn vc1b_refused_commits_are_retained_and_draining_stays_within_the_record() {
        let mut admin = MountAdmin::new();
        let mut t = table(4);
        let mut k = kernel(Ok(()));
        for i in 0..MAX_RETAINED_CAPS as u64 {
            if !admin.has_room() {
                break;
            }
            admin.stage(
                1,
                VFS_OP_MOUNT_STAGE,
                Some(100 + i),
                &stage_payload(0, 9, b"/initramfs"),
            );
            let out = admin.commit(1, b"/initramfs", &mut t, &mut k);
            assert_eq!(out.status, VFS_MOUNT_STATUS_ERR_DUPLICATE);
            assert_eq!(out.retained_cap, Some(100 + i));
        }
        // Every refused capability is in the record; nothing was routed; draining has stopped
        // before a refusal could find the record full.
        let (retained, overflow) = admin.retained();
        assert_eq!(overflow, 0);
        assert_eq!(retained.len(), MAX_RETAINED_CAPS);
        assert!(!admin.has_room());
        assert_eq!(t.entries.len(), 1);
    }

    #[test]
    fn vc1b_unrecordable_stages_keep_their_cap_and_the_table_bounds_draining() {
        let mut admin = MountAdmin::new();
        // Wrong opcode with a cap, malformed payload with a cap: retained, not lost.
        assert!(matches!(
            admin.stage(1, VFS_OP_MOUNT_REGISTER, Some(11), b"x"),
            StageOutcome::Retained {
                backend_cap: 11,
                ..
            }
        ));
        assert!(matches!(
            admin.stage(1, VFS_OP_MOUNT_STAGE, Some(12), b"short"),
            StageOutcome::Retained {
                backend_cap: 12,
                ..
            }
        ));
        // No cap: nothing to account for.
        assert!(matches!(
            admin.stage(1, VFS_OP_MOUNT_STAGE, None, &stage_payload(0, 9, b"/a")),
            StageOutcome::Ignored { .. }
        ));
        // A duplicate stage keeps the first and retains the second's cap.
        admin.stage(1, VFS_OP_MOUNT_STAGE, Some(20), &stage_payload(0, 9, b"/a"));
        assert!(matches!(
            admin.stage(1, VFS_OP_MOUNT_STAGE, Some(21), &stage_payload(0, 9, b"/a")),
            StageOutcome::Retained {
                backend_cap: 21,
                ..
            }
        ));
        assert_eq!(admin.retained(), (&[11u64, 12, 21][..], 0));
        let mut i = 0u8;
        while admin.has_room() {
            let p = [b'/', b'b' + i];
            admin.stage(
                1,
                VFS_OP_MOUNT_STAGE,
                Some(30 + u64::from(i)),
                &stage_payload(0, 9, &p),
            );
            i += 1;
        }
        assert!(admin.staged_count() + admin.retained().0.len() <= MAX_RETAINED_CAPS);
    }

    /// The retired numeric form carries a sender-local number as authority; its status is the
    /// explicit refusal VFS returns for it.
    #[test]
    fn vc1b_retired_numeric_register_has_a_distinct_permission_status() {
        let (b, n) = MountRegisterArgs {
            backend_send_cap: 65551,
            flags: 0,
            prefix: b"/ram",
        }
        .encode()
        .expect("encode");
        assert!(MountRegisterArgs::decode(&b[..n]).is_some());
        assert_ne!(VFS_MOUNT_STATUS_ERR_PERMISSION, VFS_MOUNT_STATUS_OK);
        assert_ne!(
            VFS_MOUNT_STATUS_ERR_PERMISSION,
            VFS_MOUNT_STATUS_ERR_NOT_STAGED
        );
    }
}
