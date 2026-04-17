// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use crate::kernel::boot::{KernelError, KernelState};
use crate::kernel::capabilities::CapId;
use crate::kernel::ipc::Message;
use crate::kernel::process::ProcessManagerError;
use crate::kernel::process::{ProcessService, SpawnV2Result, WaitPidV2Result};
use crate::runtime::SharedKernel;
use crate::services::common::service::{RequestResponseService, run_typed_request_loop};
use yarm_ipc_abi::process_abi::{
    PROC_OP_EXIT, PROC_OP_SPAWN_V2, PROC_OP_SPAWN_V3, PROC_OP_WAITPID_V2, SpawnV2Args,
    SpawnV3Args, WaitPidV2Args,
};

const PROCESS_MANAGER_ROUNDTRIP_RECV_TIMEOUT_TICKS: u64 = 1;
const MAX_EXEC_LOAD_SEGMENTS: usize = 8;
const MAX_EXEC_STACK_BYTES: usize = 4096;
const MAX_EXEC_ARGV: usize = 16;
const MAX_EXEC_ENVP: usize = 16;
const AUXV_AT_NULL: u64 = 0;
const AUXV_AT_PHDR: u64 = 3;
const AUXV_AT_PHENT: u64 = 4;
const AUXV_AT_PHNUM: u64 = 5;
const AUXV_AT_PAGESZ: u64 = 6;
const AUXV_AT_ENTRY: u64 = 9;
const ELF64_PHDR_SIZE: usize = 56;
const PT_LOAD: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecLoadSegment {
    pub file_offset: u64,
    pub virt_addr: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecInitialStack {
    pub stack_pointer: u64,
    pub used_bytes: usize,
    pub image: [u8; MAX_EXEC_STACK_BYTES],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecLaunchImage {
    pub image_id: u64,
    pub entry: u64,
    pub phdr_addr: u64,
    pub phdr_entry_size: u16,
    pub phdr_count: u16,
    pub load_segment_count: usize,
    pub load_segments: [Option<ExecLoadSegment>; MAX_EXEC_LOAD_SEGMENTS],
    pub initial_stack: ExecInitialStack,
}

fn read_u16_le(image: &[u8], offset: usize) -> Result<u16, ProcessManagerError> {
    let end = offset
        .checked_add(2)
        .ok_or(ProcessManagerError::Malformed)?;
    let bytes = image
        .get(offset..end)
        .ok_or(ProcessManagerError::Malformed)?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(bytes);
    Ok(u16::from_le_bytes(raw))
}

fn read_u32_le(image: &[u8], offset: usize) -> Result<u32, ProcessManagerError> {
    let end = offset
        .checked_add(4)
        .ok_or(ProcessManagerError::Malformed)?;
    let bytes = image
        .get(offset..end)
        .ok_or(ProcessManagerError::Malformed)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

fn read_u64_le(image: &[u8], offset: usize) -> Result<u64, ProcessManagerError> {
    let end = offset
        .checked_add(8)
        .ok_or(ProcessManagerError::Malformed)?;
    let bytes = image
        .get(offset..end)
        .ok_or(ProcessManagerError::Malformed)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(raw))
}

fn parse_exec_load_segments(
    image: &[u8],
) -> Result<
    (
        u64,
        u16,
        u16,
        [Option<ExecLoadSegment>; MAX_EXEC_LOAD_SEGMENTS],
        usize,
    ),
    ProcessManagerError,
> {
    if image.len() < 64 {
        return Err(ProcessManagerError::Malformed);
    }
    let phoff = read_u64_le(image, 32)? as usize;
    let phentsize = read_u16_le(image, 54)? as usize;
    let phnum = read_u16_le(image, 56)? as usize;
    if phnum == 0 || phentsize < ELF64_PHDR_SIZE {
        return Err(ProcessManagerError::Malformed);
    }
    let ph_table_size = phnum
        .checked_mul(phentsize)
        .ok_or(ProcessManagerError::Malformed)?;
    let ph_end = phoff
        .checked_add(ph_table_size)
        .ok_or(ProcessManagerError::Malformed)?;
    if ph_end > image.len() {
        return Err(ProcessManagerError::Malformed);
    }

    let mut count = 0usize;
    let mut segments = [None; MAX_EXEC_LOAD_SEGMENTS];
    for idx in 0..phnum {
        let base = phoff
            .checked_add(
                idx.checked_mul(phentsize)
                    .ok_or(ProcessManagerError::Malformed)?,
            )
            .ok_or(ProcessManagerError::Malformed)?;
        let p_type = read_u32_le(image, base)?;
        if p_type != PT_LOAD {
            continue;
        }
        if count >= MAX_EXEC_LOAD_SEGMENTS {
            return Err(ProcessManagerError::TableFull);
        }
        let segment = ExecLoadSegment {
            flags: read_u32_le(image, base + 4)?,
            file_offset: read_u64_le(image, base + 8)?,
            virt_addr: read_u64_le(image, base + 16)?,
            file_size: read_u64_le(image, base + 32)?,
            mem_size: read_u64_le(image, base + 40)?,
        };
        if segment.file_size > segment.mem_size {
            return Err(ProcessManagerError::Malformed);
        }
        let seg_end = segment
            .file_offset
            .checked_add(segment.file_size)
            .ok_or(ProcessManagerError::Malformed)?;
        if seg_end as usize > image.len() {
            return Err(ProcessManagerError::Malformed);
        }
        segments[count] = Some(segment);
        count += 1;
    }
    if count == 0 {
        return Err(ProcessManagerError::Malformed);
    }

    let phdr_addr = read_u64_le(image, 32)?;
    Ok((phdr_addr, phentsize as u16, phnum as u16, segments, count))
}

fn build_exec_initial_stack(
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
    entry: u64,
    phdr_addr: u64,
    phdr_entry_size: u16,
    phdr_count: u16,
) -> Result<ExecInitialStack, ProcessManagerError> {
    if argv.len() > MAX_EXEC_ARGV || envp.len() > MAX_EXEC_ENVP || stack_top == 0 {
        return Err(ProcessManagerError::Malformed);
    }
    let mut image = [0u8; MAX_EXEC_STACK_BYTES];
    let mut cursor = MAX_EXEC_STACK_BYTES;
    let stack_base = stack_top
        .checked_sub(MAX_EXEC_STACK_BYTES as u64)
        .ok_or(ProcessManagerError::Malformed)?;

    fn push_bytes(
        image: &mut [u8; MAX_EXEC_STACK_BYTES],
        cursor: &mut usize,
        bytes: &[u8],
    ) -> Result<(), ProcessManagerError> {
        if *cursor < bytes.len() {
            return Err(ProcessManagerError::TableFull);
        }
        *cursor -= bytes.len();
        image[*cursor..*cursor + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    fn push_u64(
        image: &mut [u8; MAX_EXEC_STACK_BYTES],
        cursor: &mut usize,
        value: u64,
    ) -> Result<(), ProcessManagerError> {
        push_bytes(image, cursor, &value.to_le_bytes())
    }

    let mut argv_ptrs = [0u64; MAX_EXEC_ARGV];
    for (idx, arg) in argv.iter().enumerate().rev() {
        push_bytes(&mut image, &mut cursor, &[0])?;
        push_bytes(&mut image, &mut cursor, arg)?;
        argv_ptrs[idx] = stack_base + cursor as u64;
    }
    let mut envp_ptrs = [0u64; MAX_EXEC_ENVP];
    for (idx, env) in envp.iter().enumerate().rev() {
        push_bytes(&mut image, &mut cursor, &[0])?;
        push_bytes(&mut image, &mut cursor, env)?;
        envp_ptrs[idx] = stack_base + cursor as u64;
    }

    cursor &= !0xFusize;
    push_u64(&mut image, &mut cursor, AUXV_AT_NULL)?;
    push_u64(&mut image, &mut cursor, 0)?;
    for (key, value) in [
        (AUXV_AT_ENTRY, entry),
        (AUXV_AT_PAGESZ, crate::kernel::vm::PAGE_SIZE as u64),
        (AUXV_AT_PHNUM, phdr_count as u64),
        (AUXV_AT_PHENT, phdr_entry_size as u64),
        (AUXV_AT_PHDR, phdr_addr),
    ]
    .into_iter()
    .rev()
    {
        push_u64(&mut image, &mut cursor, value)?;
        push_u64(&mut image, &mut cursor, key)?;
    }

    push_u64(&mut image, &mut cursor, 0)?;
    for ptr in envp_ptrs.iter().take(envp.len()).rev() {
        push_u64(&mut image, &mut cursor, *ptr)?;
    }
    push_u64(&mut image, &mut cursor, 0)?;
    for ptr in argv_ptrs.iter().take(argv.len()).rev() {
        push_u64(&mut image, &mut cursor, *ptr)?;
    }
    push_u64(&mut image, &mut cursor, argv.len() as u64)?;
    cursor &= !0xFusize;

    Ok(ExecInitialStack {
        stack_pointer: stack_base + cursor as u64,
        used_bytes: MAX_EXEC_STACK_BYTES - cursor,
        image,
    })
}

pub fn load_exec_image(
    image_id: u64,
    image: &[u8],
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<ExecLaunchImage, ProcessManagerError> {
    let info = crate::kernel::process::ElfImageInfo::parse(image_id, image)?;
    let (phdr_addr, phdr_entry_size, phdr_count, load_segments, load_segment_count) =
        parse_exec_load_segments(image)?;
    let initial_stack = build_exec_initial_stack(
        stack_top,
        argv,
        envp,
        info.entry,
        phdr_addr,
        phdr_entry_size,
        phdr_count,
    )?;
    Ok(ExecLaunchImage {
        image_id,
        entry: info.entry,
        phdr_addr,
        phdr_entry_size,
        phdr_count,
        load_segment_count,
        load_segments,
        initial_stack,
    })
}

pub fn load_exec_image_from_reader<'a, F>(
    image_id: u64,
    stack_top: u64,
    argv: &[&[u8]],
    envp: &[&[u8]],
    mut read_image: F,
) -> Result<ExecLaunchImage, ProcessManagerError>
where
    F: FnMut(u64) -> Result<&'a [u8], ProcessManagerError>,
{
    let image = read_image(image_id)?;
    load_exec_image(image_id, image, stack_top, argv, envp)
}

impl RequestResponseService for ProcessService {
    type Error = crate::kernel::process::ProcessManagerError;

    fn service_name(&self) -> &'static str {
        "process_manager"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        ProcessService::handle(self, request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessManagerLoopSummary {
    pub spawned_pid: u64,
    pub waited_pid: u64,
    pub waited_exit: u64,
    pub handled: usize,
}

fn map_kernel_ipc_err<T>(result: Result<T, KernelError>) -> Result<T, ProcessManagerError> {
    result.map_err(|_| ProcessManagerError::Malformed)
}

fn map_kernel_ipc_error(_: KernelError) -> ProcessManagerError {
    ProcessManagerError::Malformed
}

fn roundtrip_ipc(
    kernel: &mut KernelState,
    service: &mut ProcessService,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    client_recv_cap: CapId,
    request: Message,
) -> Result<Message, ProcessManagerError> {
    roundtrip_call_reply_with_budget(
        kernel,
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        request,
        PROCESS_MANAGER_ROUNDTRIP_RECV_TIMEOUT_TICKS,
    )
}

fn roundtrip_call_reply_with_budget(
    kernel: &mut KernelState,
    service: &mut ProcessService,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    client_recv_cap: CapId,
    request: Message,
    recv_timeout_ticks: u64,
) -> Result<Message, ProcessManagerError> {
    crate::services::control_plane::ipc_roundtrip::roundtrip_call_reply_with_budget(
        kernel,
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        request,
        recv_timeout_ticks,
        map_kernel_ipc_error,
        || ProcessManagerError::Malformed,
        || ProcessManagerError::Malformed,
    )
}

fn spawn_request_message(
    parent_pid: u64,
    image_id: u64,
    requested_cnode_slots: Option<usize>,
) -> Result<Message, ProcessManagerError> {
    if let Some(slots) = requested_cnode_slots {
        let slots = u64::try_from(slots).map_err(|_| ProcessManagerError::Malformed)?;
        return Message::with_header(
            0,
            PROC_OP_SPAWN_V3,
            0,
            None,
            &SpawnV3Args::new(parent_pid, image_id, slots).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed);
    }
    Message::with_header(
        0,
        PROC_OP_SPAWN_V2,
        0,
        None,
        &SpawnV2Args::new(parent_pid, image_id).encode(),
    )
    .map_err(|_| ProcessManagerError::Malformed)
}

pub fn run_request_loop(
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    let replies = run_typed_request_loop(
        service,
        [Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(parent_pid, image_id).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?],
    )?;
    let spawn_reply = replies[0];
    let spawned = SpawnV2Result::decode(spawn_reply.as_slice())?;

    let _ = run_typed_request_loop(
        service,
        [Message::with_header(
            spawned.pid.0,
            PROC_OP_EXIT,
            0,
            None,
            &exit_code.to_le_bytes(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?],
    )?;

    let wait_reply = run_typed_request_loop(
        service,
        [Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(parent_pid, spawned.pid.0).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?],
    )?[0];
    let waited = WaitPidV2Result::decode(wait_reply.as_slice())?;

    Ok(ProcessManagerLoopSummary {
        spawned_pid: spawned.pid.0,
        waited_pid: waited.waited_pid.0,
        waited_exit: waited.exit_code,
        handled: service.handled_count(),
    })
}

pub fn run_request_loop_over_kernel_ipc(
    kernel: &mut KernelState,
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    run_request_loop_over_kernel_ipc_with_requested_cnode_slots(
        kernel, service, parent_pid, image_id, exit_code, None,
    )
}

fn run_request_loop_over_kernel_ipc_with_requested_cnode_slots(
    kernel: &mut KernelState,
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
    requested_cnode_slots: Option<usize>,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    let (_, client_send_cap, server_recv_cap) = map_kernel_ipc_err(kernel.create_endpoint(8))?;
    let (_, _, client_recv_cap) = map_kernel_ipc_err(kernel.create_endpoint(8))?;

    let spawn_reply = roundtrip_ipc(
        kernel,
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        spawn_request_message(parent_pid, image_id, requested_cnode_slots)?,
    )?;
    let spawned = SpawnV2Result::decode(spawn_reply.as_slice())?;
    if let Some(requested_slots) = requested_cnode_slots {
        kernel
            .control_plane_set_process_cnode_slots_via_syscall(spawned.pid.0, requested_slots)
            .map_err(|_| ProcessManagerError::Malformed)?;
    }

    let _ = roundtrip_ipc(
        kernel,
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        Message::with_header(
            spawned.pid.0,
            PROC_OP_EXIT,
            0,
            None,
            &exit_code.to_le_bytes(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?,
    )?;

    let wait_reply = roundtrip_ipc(
        kernel,
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(parent_pid, spawned.pid.0).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed)?,
    )?;
    let waited = WaitPidV2Result::decode(wait_reply.as_slice())?;

    Ok(ProcessManagerLoopSummary {
        spawned_pid: spawned.pid.0,
        waited_pid: waited.waited_pid.0,
        waited_exit: waited.exit_code,
        handled: service.handled_count(),
    })
}

pub fn run_request_loop_over_shared_kernel_with_cnode_resize(
    kernel: &SharedKernel,
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
    requested_cnode_slots: usize,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    kernel.with(|state| {
        run_request_loop_over_kernel_ipc_with_requested_cnode_slots(
            state,
            service,
            parent_pid,
            image_id,
            exit_code,
            Some(requested_cnode_slots),
        )
    })
}

pub fn run() {
    let mut service = ProcessService::new();
    let summary = run_request_loop(&mut service, 1, 42, 0).expect("process-manager loop");

    crate::yarm_log!(
        "process-manager request-loop ready: pid={}, exit_code={}, handled={}",
        summary.waited_pid,
        summary.waited_exit,
        summary.handled
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::boot::Bootstrap;
    use crate::kernel::task::TaskClass;

    fn synthetic_elf_image(entry: u64) -> [u8; 160] {
        let mut image = [0u8; 160];
        image[..4].copy_from_slice(b"\x7FELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        image[7] = 0;
        image[16..18].copy_from_slice(&2u16.to_le_bytes());
        image[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        image[20..24].copy_from_slice(&1u32.to_le_bytes());
        image[24..32].copy_from_slice(&entry.to_le_bytes());
        image[32..40].copy_from_slice(&64u64.to_le_bytes());
        image[52..54].copy_from_slice(&64u16.to_le_bytes());
        image[54..56].copy_from_slice(&56u16.to_le_bytes());
        image[56..58].copy_from_slice(&1u16.to_le_bytes());
        let ph = 64usize;
        image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
        image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes());
        image[ph + 8..ph + 16].copy_from_slice(&120u64.to_le_bytes());
        image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes());
        image[ph + 24..ph + 32].copy_from_slice(&0u64.to_le_bytes());
        image[ph + 32..ph + 40].copy_from_slice(&16u64.to_le_bytes());
        image[ph + 40..ph + 48].copy_from_slice(&32u64.to_le_bytes());
        image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        image
    }

    #[test]
    fn process_manager_request_loop_entrypoint_runs_spawn_and_wait() {
        let mut service = ProcessService::new();
        let summary = run_request_loop(&mut service, 7, 42, 9).expect("loop");

        assert_eq!(summary.spawned_pid, summary.waited_pid);
        assert_eq!(summary.waited_exit, 9);
        assert_eq!(summary.handled, 3);
    }

    #[test]
    fn process_manager_kernel_ipc_request_loop_runs_spawn_and_wait() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let mut service = ProcessService::new();
        let summary = run_request_loop_over_kernel_ipc(&mut kernel, &mut service, 7, 42, 9)
            .expect("kernel ipc loop");

        assert_eq!(summary.spawned_pid, summary.waited_pid);
        assert_eq!(summary.waited_exit, 9);
        assert_eq!(summary.handled, 3);
    }

    #[test]
    fn process_manager_shared_kernel_path_can_resize_spawned_process_cnode() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("kernel init"));
        kernel.with(|state| {
            state
                .register_task_with_class(960, TaskClass::SystemServer)
                .expect("system-server");
            state.enqueue_current_cpu(960).expect("enqueue");
            state.dispatch_next_task().expect("dispatch");
            if state.current_tid() != Some(960) {
                state.yield_current().expect("switch");
            }
        });

        let mut service = ProcessService::new();
        let requested = kernel.with(|state| {
            state
                .runtime_capacity_config()
                .default_cnode_slot_capacity
                .saturating_add(4)
        });
        let summary = run_request_loop_over_shared_kernel_with_cnode_resize(
            &kernel,
            &mut service,
            7,
            42,
            9,
            requested,
        )
        .expect("shared-kernel loop");

        let cnode_slots = kernel.with(|state| {
            let cnode = state
                .process_cnode_for_pid(summary.spawned_pid)
                .expect("spawned cnode");
            state.cnode_slot_capacity(cnode)
        });
        assert_eq!(cnode_slots, Some(requested));
    }

    #[test]
    fn process_manager_shared_kernel_requested_resize_is_denied_without_system_server_context() {
        let kernel = SharedKernel::new(Bootstrap::init().expect("kernel init"));
        let mut service = ProcessService::new();
        let err = run_request_loop_over_shared_kernel_with_cnode_resize(
            &kernel,
            &mut service,
            7,
            42,
            9,
            32,
        )
        .expect_err("unprivileged resize must fail");
        assert_eq!(err, ProcessManagerError::Malformed);
    }

    #[test]
    fn process_manager_kernel_ipc_v2_spawn_path_does_not_create_process_cnode_resize_side_effect() {
        let mut kernel = Bootstrap::init().expect("kernel init");
        let mut service = ProcessService::new();
        let summary = run_request_loop_over_kernel_ipc(&mut kernel, &mut service, 7, 42, 9)
            .expect("kernel ipc loop");
        assert!(kernel.process_cnode_for_pid(summary.spawned_pid).is_none());
    }

    #[test]
    fn process_manager_source_guardrail_prefers_budgeted_timed_receive_path() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("roundtrip_call_reply_with_budget"),
            "process-manager migration should keep budgeted call/reply helper"
        );
        assert!(
            src.contains("ipc_recv_with_deadline"),
            "process-manager migration should keep timed receive call-sites"
        );
        assert!(
            src.contains("ipc_reply("),
            "process-manager migration should keep reply-cap reply path"
        );
        assert!(
            src.contains("PROC_OP_SPAWN_V3"),
            "process-manager migration should include v3 spawn path for requested cnode slots"
        );
    }

    #[test]
    fn minimal_elf_loader_builds_launch_image_and_initial_stack() {
        let image = synthetic_elf_image(0x401000);
        let exec = load_exec_image(
            77,
            &image,
            0x8000_0000,
            &[b"/bin/init", b"--safe"],
            &[b"PATH=/bin"],
        )
        .expect("exec image");

        assert_eq!(exec.image_id, 77);
        assert_eq!(exec.entry, 0x401000);
        assert_eq!(exec.load_segment_count, 1);
        let seg = exec.load_segments[0].expect("segment");
        assert_eq!(seg.file_offset, 120);
        assert_eq!(seg.file_size, 16);
        assert_eq!(seg.mem_size, 32);
        assert_eq!(seg.flags, 5);
        assert!(exec.initial_stack.stack_pointer <= 0x8000_0000);
        assert!(exec.initial_stack.used_bytes > 0);
    }

    #[test]
    fn minimal_elf_loader_supports_filesystem_reader_callback() {
        let image = synthetic_elf_image(0x402000);
        let exec = load_exec_image_from_reader(
            91,
            0x8100_0000,
            &[b"/sbin/proc_mgr"],
            &[b"HOME=/"],
            |id| {
                if id == 91 {
                    Ok(&image)
                } else {
                    Err(ProcessManagerError::UnknownProcess)
                }
            },
        )
        .expect("exec image");
        assert_eq!(exec.entry, 0x402000);
        assert_eq!(exec.load_segment_count, 1);
    }
}
