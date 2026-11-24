// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan
//! VFS-CORRECT1B — the live mount-authority witness (init, `vfs-mount-witness`).
//!
//! Runs in place of init's ordinary `/ram` registration, through the production registration
//! client (`register_mount_with_vfs`: stage on the admin endpoint, commit on the public one), the
//! resident VFS, and `ramfs_srv`, which seeds `/ram/vc1b-witness` (192 bytes,
//! `byte[i] = (i * 37 + 11) mod 256`) under the same feature. Each case logs one `VC1B_CASE` line;
//! the grader also reads VFS's own lines (`VFS_MOUNT_STAGE_RECEIVED`, `VFS_MOUNT_PUBLISHED`,
//! `VFS_FORWARD_CALL`) to see which identity each side used.
//!
//! 1. `ordinary_register` — the retired numeric form on the public endpoint: status 5.
//! 2. `ordinary_commit`   — a commit with nothing staged: status 6.
//! 3. `unsupported_flags` — staged with flag 0x8: status 8 (VFS retains its copy).
//! 4. `stale_handle`      — init's backend identity with its generation advanced by one: the
//!    kernel refuses the NR 1 send at init (`InvalidCapability`); nothing reaches VFS.
//! 5. `truncated_handle`  — the same identity cut to 32 bits; refused like 4 whenever the real
//!    identity exceeds 32 bits (`cap-gen-witness`), and identical to it otherwise (`applicable=0`).
//! 6. `duplicate`         — staged for `/initramfs`: status 1 (VFS retains its copy).
//! 7. `authorized`        — staged for `/ram`: status 0.
//!
//! Then the route: `/ram/vc1b-witness` opened and read through VFS into sentinel buffers (104
//! then 88 bytes, then end of file), every byte compared with the formula, tails untouched, close;
//! `ordinary_register` once more (status 5), and a fresh open+read of 4 bytes still works.
//! A capability transfer delegates (the sender keeps its copy), so init stages the same identity
//! each time.

use yarm_ipc_abi::vfs_abi::{
    MountRegisterArgs, VFS_MOUNT_STATUS_ERR_DUPLICATE, VFS_MOUNT_STATUS_ERR_NOT_STAGED,
    VFS_MOUNT_STATUS_ERR_PERMISSION, VFS_MOUNT_STATUS_ERR_UNSUPPORTED_FLAGS, VFS_MOUNT_STATUS_OK,
    VFS_OP_MOUNT_COMMIT, VFS_OP_MOUNT_REGISTER,
};
use yarm_user_rt::vfs_client::{vfs_close, vfs_openat, vfs_read};

const PATH: &[u8] = b"/ram/vc1b-witness";
const LEN: usize = 192;
const SENTINEL: u8 = 0x5A;
/// Prefixes used only by the two sender-side refusals: the core smokes' blocker scan excludes
/// `INIT_MOUNT_STAGE_REFUSED prefix=<one of these> err=InvalidCapability` and nothing else, so a
/// stage refusal anywhere in production still blocks.
const STALE_PREFIX: &[u8] = b"/vc1b-stale";
const TRUNC_PREFIX: &[u8] = b"/vc1b-trunc";

fn byte(i: usize) -> u8 {
    ((i * 37 + 11) & 0xFF) as u8
}

fn case(name: &str, expect: Option<u32>, got: &super::MountRegistration, fails: &mut u32) {
    yarm_user_rt::user_log!(
        "VC1B_CASE name={} expect={} status={} reason={}",
        name,
        expect.map(i64::from).unwrap_or(-1),
        got.status.map(i64::from).unwrap_or(-1),
        got.reason
    );
    if got.status != expect {
        *fails += 1;
    }
}

fn refused_at_sender(
    name: &str,
    cap: u64,
    applicable: bool,
    got: &super::MountRegistration,
    fails: &mut u32,
) {
    yarm_user_rt::user_log!(
        "VC1B_CASE name={} expect=stage_refused status={} reason={} cap={} applicable={}",
        name,
        got.status.map(i64::from).unwrap_or(-1),
        got.reason,
        cap,
        applicable as u8
    );
    if applicable && got.reason != "stage_refused" {
        *fails += 1;
    }
}

fn ordinary_register(vfs: u64, reply: u64, backend: u64) -> super::MountRegistration {
    let (payload, len) = MountRegisterArgs {
        backend_send_cap: backend,
        flags: 0,
        prefix: b"/ram",
    }
    .encode()
    .expect("encode");
    let msg =
        yarm_user_rt::ipc::Message::with_header(0, VFS_OP_MOUNT_REGISTER, 0, None, &payload[..len])
            .expect("msg");
    super::mount_admin_call(vfs, reply, &msg)
}

/// Run the cell. Returns whether `/ram` ended up registered.
pub(super) fn run(vfs: u64, admin: Option<u64>, reply: u64, ramfs: u64) -> bool {
    let mut fails = 0u32;
    yarm_user_rt::user_log!(
        "VC1B_BEGIN admin_present={} init_admin_cap={} init_ramfs_cap={}",
        admin.is_some() as u8,
        admin.unwrap_or(0),
        ramfs
    );

    // 1. The public endpoint alone: the retired numeric form, naming init's own slot.
    let r = ordinary_register(vfs, reply, ramfs);
    case(
        "ordinary_register",
        Some(VFS_MOUNT_STATUS_ERR_PERMISSION),
        &r,
        &mut fails,
    );

    // 2. A commit with nothing staged.
    let commit = yarm_user_rt::ipc::Message::with_header(0, VFS_OP_MOUNT_COMMIT, 0, None, b"/ram")
        .expect("msg");
    let r = super::mount_admin_call(vfs, reply, &commit);
    case(
        "ordinary_commit",
        Some(VFS_MOUNT_STATUS_ERR_NOT_STAGED),
        &r,
        &mut fails,
    );

    // 3. Unsupported flags: refused before the handshake.
    let r = super::register_mount_with_vfs(vfs, admin, reply, ramfs, 0x8, b"/ram");
    case(
        "unsupported_flags",
        Some(VFS_MOUNT_STATUS_ERR_UNSUPPORTED_FLAGS),
        &r,
        &mut fails,
    );

    // 4. A stale handle: init's slot, one generation on. The kernel refuses the send at init.
    let stale = ramfs.wrapping_add(1 << 16);
    let r = super::register_mount_with_vfs(vfs, admin, reply, stale, 0, STALE_PREFIX);
    refused_at_sender("stale_handle", stale, true, &r, &mut fails);

    // 5. A truncated handle: the identity cut to 32 bits (a real change only above 32 bits).
    let truncated = ramfs & 0xFFFF_FFFF;
    let applicable = truncated != ramfs;
    if applicable {
        let r = super::register_mount_with_vfs(vfs, admin, reply, truncated, 0, TRUNC_PREFIX);
        refused_at_sender("truncated_handle", truncated, true, &r, &mut fails);
    } else {
        yarm_user_rt::user_log!(
            "VC1B_CASE name=truncated_handle expect=stage_refused status=-1 reason=not_applicable cap={} applicable=0",
            truncated
        );
    }

    // 6. Duplicate prefix: refused before the handshake.
    let r = super::register_mount_with_vfs(vfs, admin, reply, ramfs, 0, b"/initramfs");
    case(
        "duplicate",
        Some(VFS_MOUNT_STATUS_ERR_DUPLICATE),
        &r,
        &mut fails,
    );

    // 7. The authorized registration.
    let r = super::register_mount_with_vfs(vfs, admin, reply, ramfs, 0, b"/ram");
    case("authorized", Some(VFS_MOUNT_STATUS_OK), &r, &mut fails);
    let registered = r.status == Some(VFS_MOUNT_STATUS_OK);
    yarm_user_rt::user_log!("VC1B_IDENTITY init_local_backend={}", ramfs);

    // The route: bytes only ramfs holds.
    // SAFETY (all VFS calls): init's own VFS send and reply endpoints.
    match unsafe { vfs_openat(vfs, reply, PATH, 0) } {
        Ok(fd) => {
            yarm_user_rt::user_log!("VC1B_OPEN ok=1 fd={}", fd);
            let mut off = 0usize;
            for (k, (size, want)) in [(200usize, 104usize), (200, 88), (64, 0)]
                .iter()
                .enumerate()
            {
                let mut region = [SENTINEL; 256];
                match unsafe { vfs_read(vfs, reply, fd, &mut region[..*size]) } {
                    Ok(n) => {
                        let bytes_ok = (0..n).all(|i| region[i] == byte(off + i));
                        let tail_ok = region[n..].iter().all(|&b| b == SENTINEL);
                        yarm_user_rt::user_log!(
                            "VC1B_READ k={} size={} n={} off={} bytes_ok={} tail_ok={}",
                            k,
                            size,
                            n,
                            off,
                            bytes_ok as u8,
                            tail_ok as u8
                        );
                        for c in (0..n).step_by(32) {
                            let end = core::cmp::min(c + 32, n);
                            let mut hex = [0u8; 64];
                            for (j, b) in region[c..end].iter().enumerate() {
                                const H: &[u8; 16] = b"0123456789abcdef";
                                hex[2 * j] = H[(b >> 4) as usize];
                                hex[2 * j + 1] = H[(b & 0xF) as usize];
                            }
                            yarm_user_rt::user_log!(
                                "VC1B_DATA k={} off={} hex={}",
                                k,
                                off + c,
                                core::str::from_utf8(&hex[..2 * (end - c)]).unwrap_or("?")
                            );
                        }
                        if n != *want || !bytes_ok || !tail_ok {
                            fails += 1;
                        }
                        off += n;
                    }
                    Err(e) => {
                        yarm_user_rt::user_log!("VC1B_READ k={} size={} ok=0 err={:?}", k, size, e);
                        fails += 1;
                    }
                }
            }
            let closed = unsafe { vfs_close(vfs, reply, fd) }.is_ok();
            yarm_user_rt::user_log!("VC1B_CLOSE ok={} total={}", closed as u8, off);
            if !closed || off != LEN {
                fails += 1;
            }
        }
        Err(e) => {
            yarm_user_rt::user_log!("VC1B_OPEN ok=0 err={:?}", e);
            fails += 1;
        }
    }

    // An ordinary client is still refused, and the route still works afterwards.
    let r = ordinary_register(vfs, reply, ramfs);
    case(
        "ordinary_register_after",
        Some(VFS_MOUNT_STATUS_ERR_PERMISSION),
        &r,
        &mut fails,
    );
    let recovered = match unsafe { vfs_openat(vfs, reply, PATH, 0) } {
        Ok(fd) => {
            let mut head = [SENTINEL; 4];
            let n = unsafe { vfs_read(vfs, reply, fd, &mut head) };
            let closed = unsafe { vfs_close(vfs, reply, fd) }.is_ok();
            matches!(n, Ok(4)) && (0..4).all(|i| head[i] == byte(i)) && closed
        }
        Err(_) => false,
    };
    yarm_user_rt::user_log!("VC1B_RECOVER ok={}", recovered as u8);
    if !recovered {
        fails += 1;
    }
    yarm_user_rt::user_log!(
        "VC1B_END fails={} result={}",
        fails,
        if fails == 0 { "ok" } else { "fail" }
    );
    registered
}
