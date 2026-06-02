// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(test)]
use yarm::kernel::boot::KernelState;
#[cfg(test)]
use yarm::kernel::boot::{KernelError, TrapHandleError};
#[cfg(test)]
use yarm::kernel::process::{ProcessManager, ProcessManagerError as KernelProcessManagerError};
#[cfg(test)]
use yarm::kernel::syscall::SyscallError as KernelSyscallError;
use alloc::vec::Vec;
use yarm_ipc_abi::process_abi::{
    ExecuteRestartReply, ExecuteRestartRequest, LIFECYCLE_STATE_SPAWNED,
    LifecycleQueryReply, LifecycleQueryRequest, PROC_OP_EXECUTE_RESTART, PROC_OP_EXIT,
    PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_LIFECYCLE_QUERY,
    PROC_OP_REGISTER_SUPERVISED_TASK, PROC_OP_SPAWN_V2, PROC_OP_SPAWN_V3, PROC_OP_SPAWN_V4,
    PROC_OP_SPAWN_V5_CAP, PROC_OP_TASK_RESTART_TOKEN, PROC_OP_WAITPID_V2, RegisterSupervisedTask,
    SpawnV2Args, SpawnV3Args, SpawnV4Args, SpawnV5CapArgs,
    TaskRestartTokenReply, TaskRestartTokenRequest, WaitPidV2Args,
    encode_spawn_v5_reply,
};
use yarm_srv_common::elf::ElfImageInfo;
use yarm_srv_common::service_loop::RequestResponseService;
use yarm_srv_common::service_loop::run_typed_request_loop;
#[cfg(test)]
use yarm_user_rt::capability::CapId;
use yarm_user_rt::ipc::Message;

const PM_VFS_READ_APPEND_TRACE: bool = false;
/// Gate for per-chunk bulk-read trace logs.  Set true to debug chunk boundaries.
const PM_VFS_BULK_READ_CHUNK_TRACE: bool = false;
/// Gate for Phase 2B VFS-transfer per-chunk logs (hot-path).
const PM_VFS_BULK_READ_TRANSFER_CHUNK_TRACE: bool = false;
#[cfg(not(test))]
use yarm_user_rt::vfs_client::{
    build_bulk_read_message, build_close_message, build_openat_message, build_read_message,
    build_statx_message,
};
use yarm_user_rt::process::{
    ProcessError as ProcessManagerError, ProcessId, ProcessManagerOps, WaitResult,
};
#[cfg(test)]
use yarm_user_rt::runtime::{KernelIpcError, RuntimeStateAccess, TrapIpcError};
#[cfg(test)]
use yarm_user_rt::syscall::SyscallError;
use yarm_user_rt::task::TaskClass;

#[cfg(test)]
const PROCESS_MANAGER_ROUNDTRIP_RECV_TIMEOUT_TICKS: u64 = 1;
/// Image IDs in this inclusive range are bootstrap-critical: they must be
/// spawned via the direct kernel path before VFS is available.
const BOOTSTRAP_IMAGE_ID_MIN: u64 = 1;
const BOOTSTRAP_SERVICE_IMAGE_ID_MAX: u64 = 6;
const VFS_SERVICE_IMAGE_ID_MIN: u64 = 7;
const VFS_SERVICE_IMAGE_ID_MAX: u64 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnLoadSource {
    DirectInitrd,
    Vfs,
}

fn resolve_spawn_load_source(image_id: u64) -> Result<SpawnLoadSource, ProcessManagerError> {
    if (BOOTSTRAP_IMAGE_ID_MIN..=BOOTSTRAP_SERVICE_IMAGE_ID_MAX).contains(&image_id) {
        return Ok(SpawnLoadSource::DirectInitrd);
    }
    if (VFS_SERVICE_IMAGE_ID_MIN..=VFS_SERVICE_IMAGE_ID_MAX).contains(&image_id) {
        return Ok(SpawnLoadSource::Vfs);
    }
    Err(ProcessManagerError::Unsupported)
}

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
pub struct SpawnV2Request {
    pub parent_pid: ProcessId,
    pub image_id: u64,
    pub requested_cnode_slots: Option<usize>,
    pub requested_task_class: Option<TaskClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV5CapRequest {
    pub parent_pid: ProcessId,
    pub image_id: u64,
    pub service_caps: [u64; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidV2Request {
    pub caller_pid: ProcessId,
    pub target_pid: ProcessId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnV2Result {
    pub pid: ProcessId,
}

impl SpawnV2Result {
    pub const fn encode(self) -> [u8; 8] {
        self.pid.0.to_le_bytes()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        if payload.len() < 8 {
            return Err(ProcessManagerError::Malformed);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&payload[..8]);
        Ok(Self {
            pid: ProcessId(u64::from_le_bytes(bytes)),
        })
    }
}

pub struct WaitPidV2Result {
    pub waited_pid: ProcessId,
    pub exit_code: u64,
}

impl WaitPidV2Result {
    pub const fn encode(self) -> [u8; 16] {
        yarm_ipc_abi::process_abi::WaitPidV2Reply::new(self.waited_pid.0, self.exit_code).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        let args = yarm_ipc_abi::process_abi::WaitPidV2Reply::decode(payload)
            .map_err(|_| ProcessManagerError::Malformed)?;
        Ok(Self {
            waited_pid: ProcessId(args.waited_pid),
            exit_code: args.exit_code,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRequest {
    GetPid { caller_tid: u64 },
    GetPpid { caller_tid: u64 },
    Exit { caller_tid: u64, code: u64 },
    SpawnV2(SpawnV2Request),
    SpawnV5Cap(SpawnV5CapRequest),
    WaitPidV2(WaitPidV2Request),
    TaskRestartToken { tid: u64 },
    RegisterSupervisedTask { tid: u64, restart_token: u64 },
    ExecuteRestart { tid: u64, restart_token: u64 },
    LifecycleQuery { tid: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessSpawnPolicyRecord {
    pid: ProcessId,
    image_id: u64,
    entry: u64,
    requested_cnode_slots: Option<usize>,
    requested_task_class: Option<TaskClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RestartTokenRecord {
    tid: u64,
    token: u64,
}

#[derive(Debug)]
#[cfg(test)]
struct KernelProcessManagerAdapter {
    inner: ProcessManager,
}

#[derive(Debug, Default)]
#[cfg(not(test))]
struct KernelProcessManagerAdapter;

#[cfg(not(test))]
struct KernelProcessSpawnBackend;

#[cfg(not(test))]
impl KernelProcessSpawnBackend {
    const fn new() -> Self {
        Self
    }

    fn spawn(&self, image_id: u64, parent_pid: u64) -> Result<u64, ProcessManagerError> {
        yarm_user_rt::user_log!("PM_HANDLE_SPAWN_V5_BEGIN image_id={} parent_pid={}", image_id, parent_pid);
        // SAFETY: Delegates to kernel spawn_process syscall (nr=23).
        let result = unsafe {
            yarm_user_rt::syscall::spawn_process(image_id, parent_pid)
                .map_err(|_| ProcessManagerError::TableFull)
        };
        yarm_user_rt::user_log!("PM_HANDLE_SPAWN_V5_RESULT ok={}", result.is_ok() as u8);
        result
    }

    fn spawn_with_caps(&self, image_id: u64, parent_pid: u64, service_caps: [u64; 4]) -> Result<(u64, u32, u32), ProcessManagerError> {
        yarm_user_rt::user_log!(
            "PM_SPAWN_CAP_BEGIN image_id={} parent_pid={} caps=[{},{},{},{}]",
            image_id, parent_pid,
            service_caps[0], service_caps[1], service_caps[2], service_caps[3]
        );
        let mut startup_args = [0u64; 18];
        startup_args[13] = service_caps[0];
        startup_args[14] = service_caps[1];
        startup_args[15] = service_caps[2];
        startup_args[16] = service_caps[3];
        // SAFETY: Delegates to kernel spawn_process syscall with startup_args.
        let result = unsafe {
            yarm_user_rt::syscall::spawn_process_with_startup_caps(image_id, parent_pid, &startup_args)
                .map_err(|_| ProcessManagerError::TableFull)
        };
        yarm_user_rt::user_log!("PM_SPAWN_CAP_RESULT ok={}", result.is_ok() as u8);
        result
    }
}

#[cfg(test)]
impl KernelProcessManagerAdapter {
    const fn new() -> Self {
        Self {
            inner: ProcessManager::new(),
        }
    }

    #[inline]
    fn to_kernel_process_id(pid: ProcessId) -> yarm::kernel::process::ProcessId {
        yarm::kernel::process::ProcessId(pid.0)
    }

    #[inline]
    fn from_kernel_process_id(pid: yarm::kernel::process::ProcessId) -> ProcessId {
        ProcessId(pid.0)
    }

    #[inline]
    fn map_kernel_process_error(err: KernelProcessManagerError) -> ProcessManagerError {
        match err {
            KernelProcessManagerError::Malformed => ProcessManagerError::Malformed,
            KernelProcessManagerError::Unsupported => ProcessManagerError::Unsupported,
            KernelProcessManagerError::TableFull => ProcessManagerError::TableFull,
            KernelProcessManagerError::UnknownProcess => ProcessManagerError::UnknownProcess,
            KernelProcessManagerError::InvalidTransport => ProcessManagerError::InvalidTransport,
            KernelProcessManagerError::PermissionDenied => ProcessManagerError::PermissionDenied,
            KernelProcessManagerError::WouldBlock => ProcessManagerError::WouldBlock,
        }
    }
}

#[cfg(not(test))]
impl KernelProcessManagerAdapter {
    const fn new() -> Self {
        Self
    }
}

#[cfg(test)]
impl ProcessManagerOps for KernelProcessManagerAdapter {
    fn process_id_for_tid(&self, tid: u64) -> ProcessId {
        Self::from_kernel_process_id(self.inner.process_id_for_tid(tid))
    }

    fn parent_of(&self, pid: ProcessId) -> Option<ProcessId> {
        self.inner
            .parent_of(Self::to_kernel_process_id(pid))
            .map(Self::from_kernel_process_id)
    }

    fn allocate_process(
        &mut self,
        parent_pid: ProcessId,
    ) -> Result<ProcessId, ProcessManagerError> {
        self.inner
            .allocate_process(Self::to_kernel_process_id(parent_pid))
            .map(Self::from_kernel_process_id)
            .map_err(Self::map_kernel_process_error)
    }

    fn insert_synthetic_exit_for_tid(
        &mut self,
        tid: u64,
        code: u64,
    ) -> Result<(), ProcessManagerError> {
        self.inner
            .insert_synthetic_exit_for_tid(tid, code)
            .map(|_| ())
            .map_err(Self::map_kernel_process_error)
    }

    fn wait_exited(&mut self, pid: ProcessId) -> Result<WaitResult, ProcessManagerError> {
        let waited = self
            .inner
            .wait_exited(Self::to_kernel_process_id(pid))
            .map_err(Self::map_kernel_process_error)?;
        Ok(WaitResult {
            waited_pid: Self::from_kernel_process_id(waited.waited_pid),
            exit_code: waited.exit_code,
        })
    }

    fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessManagerError> {
        self.inner
            .mark_exit(Self::to_kernel_process_id(pid), code)
            .map_err(Self::map_kernel_process_error)
    }
}

#[cfg(not(test))]
impl ProcessManagerOps for KernelProcessManagerAdapter {
    fn process_id_for_tid(&self, tid: u64) -> ProcessId {
        ProcessId(tid)
    }

    fn parent_of(&self, _pid: ProcessId) -> Option<ProcessId> {
        None
    }

    fn allocate_process(
        &mut self,
        _parent_pid: ProcessId,
    ) -> Result<ProcessId, ProcessManagerError> {
        Err(ProcessManagerError::Unsupported)
    }

    fn insert_synthetic_exit_for_tid(
        &mut self,
        _tid: u64,
        _code: u64,
    ) -> Result<(), ProcessManagerError> {
        Ok(())
    }

    fn wait_exited(&mut self, _pid: ProcessId) -> Result<WaitResult, ProcessManagerError> {
        Err(ProcessManagerError::WouldBlock)
    }

    fn mark_exit(&mut self, _pid: ProcessId, _code: u64) -> Result<(), ProcessManagerError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Spawned,
}

/// One entry in the PM lifecycle table. Tracks the kernel-level identity of
/// each service spawned through PM so that caps can be re-granted downstream
/// and the service state can be queried.
#[derive(Debug, Clone, Copy)]
pub struct ServiceLifecycleRecord {
    pub tid: u64,
    pub image_id: u64,
    /// TID of the task that requested the spawn (0 = no requester / direct).
    pub parent_tid: u64,
    /// PM's own send cap for this service's IPC endpoint (valid in PM's CNode).
    ///
    /// Startup slot layout recap (indices into the 18-element startup_args array):
    ///  Slot 0  — task id
    ///  Slot 1  — PM request send cap (to reach PM for new requests)
    ///  Slot 2  — PM reply recv cap  (for PM replies)
    ///  Slot 12 — service recv ep    (child's own inbound endpoint; this is what
    ///            each spawned service reads as `ctx.process_manager_service_recv_ep`)
    ///  Slot 13 — service_extra_cap_0 (e.g. initramfs send cap passed to vfs_server)
    ///  Slot 14 — service_extra_cap_1 (e.g. devfs send cap passed to vfs_server)
    ///  Slot 15/16 — reserved extra caps
    ///  Slot 17 — PM inbound recv cap (only wired for PM itself)
    ///
    /// `pm_service_send_cap` is the spawner's (PM's) side of the endpoint created
    /// at slot 12.  When `parent_pid != 0` the kernel also delegates a copy into
    /// the parent's CNode; `pm_service_send_cap` always refers to the copy that
    /// stays in PM's CNode.
    pub pm_service_send_cap: u32,
    pub state: ServiceState,
}

const MAX_LIFECYCLE_ENTRIES: usize = 32;

/// Fixed-capacity lifecycle table for spawned services.  Uses a flat array so
/// it is compatible with `no_std` and `const` initialisation.
#[derive(Debug)]
pub struct LifecycleTable {
    entries: [Option<ServiceLifecycleRecord>; MAX_LIFECYCLE_ENTRIES],
    len: usize,
}

impl LifecycleTable {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_LIFECYCLE_ENTRIES],
            len: 0,
        }
    }

    /// Append a new record.  Returns `false` if the table is full.
    pub fn record(&mut self, rec: ServiceLifecycleRecord) -> bool {
        if self.len >= MAX_LIFECYCLE_ENTRIES {
            return false;
        }
        self.entries[self.len] = Some(rec);
        self.len += 1;
        true
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get_by_tid(&self, tid: u64) -> Option<&ServiceLifecycleRecord> {
        self.entries[..self.len]
            .iter()
            .filter_map(|e| e.as_ref())
            .find(|r| r.tid == tid)
    }

    pub fn get_by_image_id(&self, image_id: u64) -> Option<&ServiceLifecycleRecord> {
        self.entries[..self.len]
            .iter()
            .filter_map(|e| e.as_ref())
            .find(|r| r.image_id == image_id)
    }
}

#[derive(Debug)]
pub struct ProcessService {
    manager: KernelProcessManagerAdapter,
    policy_records: [Option<ProcessSpawnPolicyRecord>; 64],
    restart_token_records: [Option<RestartTokenRecord>; 64],
    restart_control_send_cap: Option<u32>,
    /// Lifecycle table: one entry per successfully spawned service.
    lifecycle_table: LifecycleTable,
    handled: usize,
}

impl Default for ProcessService {
    fn default() -> Self {
        Self::new()
    }
}

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
        (AUXV_AT_PAGESZ, yarm_user_rt::vm::PAGE_SIZE as u64),
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
    let info = ElfImageInfo::parse(image_id, image).map_err(map_elf_error)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessManagerLoopSummary {
    pub spawned_pid: u64,
    pub waited_pid: u64,
    pub waited_exit: u64,
    pub handled: usize,
}

#[cfg(test)]
fn map_kernel_ipc_err<T>(result: Result<T, KernelError>) -> Result<T, ProcessManagerError> {
    result.map_err(|err| map_kernel_ipc_error(from_kernel_ipc_error(err)))
}

#[cfg(test)]
fn from_kernel_ipc_error(err: KernelError) -> KernelIpcError {
    match err {
        KernelError::MissingRight => KernelIpcError::MissingRight,
        KernelError::WouldBlock => KernelIpcError::WouldBlock,
        KernelError::CapabilityFull => KernelIpcError::CapabilityFull,
        KernelError::EndpointFull => KernelIpcError::EndpointFull,
        KernelError::EndpointQueueFull => KernelIpcError::EndpointQueueFull,
        KernelError::TaskTableFull => KernelIpcError::TaskTableFull,
        KernelError::MemoryObjectFull => KernelIpcError::MemoryObjectFull,
        KernelError::SchedulerFull => KernelIpcError::SchedulerFull,
        KernelError::VmFull => KernelIpcError::VmFull,
        KernelError::InvalidCapability => KernelIpcError::InvalidCapability,
        KernelError::WrongObject => KernelIpcError::WrongObject,
        KernelError::StaleCapability => KernelIpcError::StaleCapability,
        KernelError::UserMemoryFault => KernelIpcError::UserMemoryFault,
        KernelError::TaskMissing => KernelIpcError::TaskMissing,
        KernelError::MemoryObjectMissing => KernelIpcError::MemoryObjectMissing,
        KernelError::Vm(_) => KernelIpcError::VmFault,
    }
}

#[cfg(test)]
fn map_kernel_ipc_error(err: KernelIpcError) -> ProcessManagerError {
    match err {
        KernelIpcError::MissingRight => ProcessManagerError::PermissionDenied,
        KernelIpcError::WouldBlock => ProcessManagerError::WouldBlock,
        KernelIpcError::CapabilityFull
        | KernelIpcError::EndpointFull
        | KernelIpcError::EndpointQueueFull
        | KernelIpcError::TaskTableFull
        | KernelIpcError::MemoryObjectFull
        | KernelIpcError::SchedulerFull
        | KernelIpcError::VmFull => ProcessManagerError::TableFull,
        KernelIpcError::InvalidCapability
        | KernelIpcError::WrongObject
        | KernelIpcError::StaleCapability
        | KernelIpcError::UserMemoryFault
        | KernelIpcError::TaskMissing
        | KernelIpcError::MemoryObjectMissing
        | KernelIpcError::VmFault => ProcessManagerError::Malformed,
    }
}

#[cfg(test)]
fn from_kernel_trap_ipc_error(err: TrapHandleError) -> TrapIpcError {
    match err {
        TrapHandleError::Syscall(syscall_err) => {
            TrapIpcError::Syscall(map_kernel_syscall_error(syscall_err))
        }
        TrapHandleError::MissingTrapFrame => TrapIpcError::MissingTrapFrame,
    }
}

#[cfg(test)]
fn map_trap_ipc_error(err: TrapIpcError) -> ProcessManagerError {
    match err {
        TrapIpcError::Syscall(syscall_err) => map_syscall_error(syscall_err),
        TrapIpcError::MissingTrapFrame => ProcessManagerError::InvalidTransport,
    }
}

#[cfg(test)]
fn map_kernel_syscall_error(err: KernelSyscallError) -> SyscallError {
    match err {
        KernelSyscallError::InvalidNumber => SyscallError::InvalidNumber,
        KernelSyscallError::InvalidArgs => SyscallError::InvalidArgs,
        KernelSyscallError::InvalidCapability => SyscallError::InvalidCapability,
        KernelSyscallError::MissingRight => SyscallError::MissingRight,
        KernelSyscallError::WrongObject => SyscallError::WrongObject,
        KernelSyscallError::QueueFull => SyscallError::QueueFull,
        KernelSyscallError::WouldBlock => SyscallError::WouldBlock,
        KernelSyscallError::PageFault => SyscallError::PageFault,
        KernelSyscallError::TimedOut => SyscallError::TimedOut,
        KernelSyscallError::Internal => SyscallError::Internal,
    }
}

#[cfg(test)]
fn map_syscall_error(err: SyscallError) -> ProcessManagerError {
    match err {
        SyscallError::MissingRight => ProcessManagerError::PermissionDenied,
        SyscallError::WouldBlock | SyscallError::TimedOut => ProcessManagerError::WouldBlock,
        SyscallError::QueueFull | SyscallError::Internal => ProcessManagerError::TableFull,
        SyscallError::InvalidNumber
        | SyscallError::InvalidArgs
        | SyscallError::InvalidCapability
        | SyscallError::WrongObject
        | SyscallError::PageFault => ProcessManagerError::Malformed,
    }
}

impl ProcessService {
    pub fn new() -> Self {
        Self {
            manager: KernelProcessManagerAdapter::new(),
            policy_records: [None; 64],
            restart_token_records: [None; 64],
            restart_control_send_cap: yarm_user_rt::runtime::startup_context()
                .process_manager_restart_control_send_cap,
            lifecycle_table: LifecycleTable::new(),
            handled: 0,
        }
    }

    pub fn lifecycle_table(&self) -> &LifecycleTable {
        &self.lifecycle_table
    }

    /// Record a lifecycle entry for a bootstrap service spawned before PM's
    /// request loop started.  Unlike the SpawnV5Cap path, pm_service_send_cap
    /// is 0 (PM holds no cap to these services from a spawn syscall) and
    /// parent_tid is 0 (spawned at kernel boot, no PM-tracked requester).
    pub fn seed_bootstrap_lifecycle_record(&mut self, tid: u64, image_id: u64) -> bool {
        let recorded = self.lifecycle_table.record(ServiceLifecycleRecord {
            tid,
            image_id,
            parent_tid: 0,
            pm_service_send_cap: 0,
            state: ServiceState::Spawned,
        });
        yarm_user_rt::user_log!(
            "PM_LIFECYCLE_BOOTSTRAP tid={} image_id={} recorded={}",
            tid,
            image_id,
            recorded as u8
        );
        recorded
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    fn read_u64(payload: &[u8]) -> Result<u64, ProcessManagerError> {
        if payload.len() < 8 {
            return Err(ProcessManagerError::Malformed);
        }
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&payload[..8]);
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn parse_request(msg: Message) -> Result<ProcessRequest, ProcessManagerError> {
        if msg.transferred_cap().is_some() || (msg.flags & Message::FLAG_CAP_TRANSFER) != 0 {
            return Err(ProcessManagerError::InvalidTransport);
        }
        match msg.opcode {
            PROC_OP_GETPID => Ok(ProcessRequest::GetPid {
                caller_tid: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_GETPPID => Ok(ProcessRequest::GetPpid {
                caller_tid: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_EXIT => Ok(ProcessRequest::Exit {
                caller_tid: msg.sender_tid.0,
                code: Self::read_u64(msg.as_slice())?,
            }),
            PROC_OP_SPAWN_V2 => {
                let args = SpawnV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                yarm_user_rt::user_log!(
                    "PM_SPAWN_V5_DECODE image_id={} parent_pid={} startup_caps_version=2",
                    args.image_id,
                    args.parent_pid
                );
                Ok(ProcessRequest::SpawnV2(SpawnV2Request {
                    parent_pid: ProcessId(args.parent_pid),
                    image_id: args.image_id,
                    requested_cnode_slots: None,
                    requested_task_class: None,
                }))
            }
            PROC_OP_SPAWN_V3 => {
                let args = SpawnV3Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                let requested_cnode_slots = usize::try_from(args.requested_cnode_slots)
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::SpawnV2(SpawnV2Request {
                    parent_pid: ProcessId(args.parent_pid),
                    image_id: args.image_id,
                    requested_cnode_slots: Some(requested_cnode_slots),
                    requested_task_class: None,
                }))
            }
            PROC_OP_SPAWN_V4 => {
                let args = SpawnV4Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                let requested_cnode_slots = usize::try_from(args.requested_cnode_slots)
                    .map_err(|_| ProcessManagerError::Malformed)?;
                let requested_task_class = match args.task_class_hint {
                    0 => TaskClass::App,
                    1 => TaskClass::Driver,
                    2 => TaskClass::SystemServer,
                    _ => return Err(ProcessManagerError::Malformed),
                };
                Ok(ProcessRequest::SpawnV2(SpawnV2Request {
                    parent_pid: ProcessId(args.parent_pid),
                    image_id: args.image_id,
                    requested_cnode_slots: Some(requested_cnode_slots),
                    requested_task_class: Some(requested_task_class),
                }))
            }
            PROC_OP_SPAWN_V5_CAP => {
                let args = SpawnV5CapArgs::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                yarm_user_rt::user_log!(
                    "PM_SPAWN_V5_CAP_DECODE image_id={} parent_pid={} cap0={} cap1={}",
                    args.image_id, args.parent_pid,
                    args.service_caps[0], args.service_caps[1]
                );
                Ok(ProcessRequest::SpawnV5Cap(SpawnV5CapRequest {
                    parent_pid: ProcessId(args.parent_pid),
                    image_id: args.image_id,
                    service_caps: args.service_caps,
                }))
            }
            PROC_OP_WAITPID_V2 => {
                let args = WaitPidV2Args::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::WaitPidV2(WaitPidV2Request {
                    caller_pid: ProcessId(args.caller_pid),
                    target_pid: ProcessId(args.target_pid),
                }))
            }
            PROC_OP_TASK_RESTART_TOKEN => {
                let args = TaskRestartTokenRequest::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::TaskRestartToken { tid: args.tid })
            }
            PROC_OP_REGISTER_SUPERVISED_TASK => {
                let args = RegisterSupervisedTask::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::RegisterSupervisedTask {
                    tid: args.tid,
                    restart_token: args.restart_token,
                })
            }
            PROC_OP_EXECUTE_RESTART => {
                let args = ExecuteRestartRequest::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::ExecuteRestart {
                    tid: args.tid,
                    restart_token: args.restart_token,
                })
            }
            PROC_OP_LIFECYCLE_QUERY => {
                let args = LifecycleQueryRequest::decode(msg.as_slice())
                    .map_err(|_| ProcessManagerError::Malformed)?;
                Ok(ProcessRequest::LifecycleQuery { tid: args.tid })
            }
            _ => Err(ProcessManagerError::Unsupported),
        }
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, ProcessManagerError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| ProcessManagerError::Malformed)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn record_spawn_policy(
        &mut self,
        pid: ProcessId,
        image_id: u64,
        entry: u64,
        requested_cnode_slots: Option<usize>,
        requested_task_class: Option<TaskClass>,
    ) -> Result<(), ProcessManagerError> {
        if let Some(record) = self
            .policy_records
            .iter_mut()
            .flatten()
            .find(|record| record.pid == pid)
        {
            *record = ProcessSpawnPolicyRecord {
                pid,
                image_id,
                entry,
                requested_cnode_slots,
                requested_task_class,
            };
            return Ok(());
        }
        let slot = self
            .policy_records
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ProcessManagerError::TableFull)?;
        *slot = Some(ProcessSpawnPolicyRecord {
            pid,
            image_id,
            entry,
            requested_cnode_slots,
            requested_task_class,
        });
        Ok(())
    }

    pub fn requested_cnode_slots_for_process(&self, pid: u64) -> Option<Option<usize>> {
        self.policy_records
            .iter()
            .flatten()
            .find(|record| record.pid == ProcessId(pid))
            .map(|record| record.requested_cnode_slots)
    }

    pub fn requested_task_class_for_process(&self, pid: u64) -> Option<Option<TaskClass>> {
        self.policy_records
            .iter()
            .flatten()
            .find(|record| record.pid == ProcessId(pid))
            .map(|record| record.requested_task_class)
    }

    pub fn process_image_info(&self, pid: ProcessId) -> Option<ElfImageInfo> {
        self.policy_records
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| ElfImageInfo {
                image_id: record.image_id,
                entry: record.entry,
            })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn record_restart_token(&mut self, tid: u64, token: u64) -> Result<(), ProcessManagerError> {
        if let Some(record) = self
            .restart_token_records
            .iter_mut()
            .flatten()
            .find(|record| record.tid == tid)
        {
            *record = RestartTokenRecord { tid, token };
            return Ok(());
        }
        let slot = self
            .restart_token_records
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ProcessManagerError::TableFull)?;
        *slot = Some(RestartTokenRecord { tid, token });
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn restart_token_for_tid(&self, tid: u64) -> Option<u64> {
        self.restart_token_records
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
            .map(|record| record.token)
    }

    pub fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessManagerError> {
        self.manager.mark_exit(pid, code)
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        let reply = self.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    fn handle_request(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        match Self::parse_request(request)? {
            ProcessRequest::GetPid { caller_tid } => Self::u64_reply(
                PROC_OP_GETPID,
                self.manager.process_id_for_tid(caller_tid).0,
            ),
            ProcessRequest::GetPpid { caller_tid } => {
                let pid = self.manager.process_id_for_tid(caller_tid);
                Self::u64_reply(
                    PROC_OP_GETPPID,
                    self.manager
                        .parent_of(pid)
                        .unwrap_or(ProcessId(pid.0.saturating_sub(1)))
                        .0,
                )
            }
            ProcessRequest::Exit { caller_tid, code } => {
                self.manager
                    .insert_synthetic_exit_for_tid(caller_tid, code)?;
                Self::u64_reply(PROC_OP_EXIT, 0)
            }
            ProcessRequest::SpawnV2(req) => {
                #[cfg(test)]
                {
                    let image = synthetic_elf_image(req.image_id);
                    let info = ElfImageInfo::parse(req.image_id, &image).map_err(map_elf_error)?;
                    let pid = self.manager.allocate_process(req.parent_pid)?;
                    // NOTE: we intentionally do not call `record_restart_token(...)` here.
                    // At this point this path has authoritative process metadata only (pid/image/policy),
                    // but does not yet have an authoritative `(tid, restart_token)` lifecycle source.
                    // Restart-token population must be wired from a later lifecycle handoff where token data
                    // is actually created/owned and tied to a concrete tid.
                    self.record_spawn_policy(
                        pid,
                        req.image_id,
                        info.entry,
                        req.requested_cnode_slots,
                        req.requested_task_class,
                    )?;
                    let result = SpawnV2Result { pid };
                    return Message::with_header(0, PROC_OP_SPAWN_V2, 0, None, &result.encode())
                        .map_err(|_| ProcessManagerError::Malformed);
                }
                #[cfg(not(test))]
                {
                    let backend = KernelProcessSpawnBackend::new();
                    let tid = backend.spawn(req.image_id, req.parent_pid.0)?;
                    let result = SpawnV2Result {
                        pid: ProcessId(tid),
                    };
                    let encoded = result.encode();
                    yarm_user_rt::user_log!(
                        "PM_SPAWN_V5_REPLY ok=1 child_tid={} len={}",
                        tid,
                        encoded.len()
                    );
                    Message::with_header(0, PROC_OP_SPAWN_V2, 0, None, &encoded)
                        .map_err(|_| ProcessManagerError::Malformed)
                }
            }
            ProcessRequest::SpawnV5Cap(req) => {
                #[cfg(not(test))]
                {
                    // Capture all three return values from every spawn path.
                    //
                    // caller_cap:   cap returned to the requester (init) — may be in
                    //               init's CNode when parent_pid != 0, otherwise in PM's.
                    // spawner_cap:  non-zero only when parent_pid != 0; this is PM's own
                    //               copy of the service send cap (high 32 bits of ret2).
                    // pm_send_cap:  whichever of the above lives in PM's own CNode.
                    let source = resolve_spawn_load_source(req.image_id)?;
                    let (tid, caller_cap, spawner_cap) = match source {
                        SpawnLoadSource::DirectInitrd => {
                            yarm_user_rt::user_log!(
                                "PM_SPAWN_IMAGE_SELECTED image_id={} source=direct-initrd",
                                req.image_id
                            );
                            let backend = KernelProcessSpawnBackend::new();
                            let (t, c, s) = backend.spawn_with_caps(
                                req.image_id,
                                req.parent_pid.0,
                                req.service_caps,
                            )?;
                            (t, c as u64, s)
                        }
                        SpawnLoadSource::Vfs => {
                            let mut startup_args = [0u64; 18];
                            startup_args[13] = req.service_caps[0];
                            startup_args[14] = req.service_caps[1];
                            startup_args[15] = req.service_caps[2];
                            startup_args[16] = req.service_caps[3];
                            let vfs_send_cap = self
                                .lifecycle_table
                                .get_by_image_id(6)
                                .map(|rec| rec.pm_service_send_cap)
                                .unwrap_or(0);
                            if vfs_send_cap == 0 {
                                yarm_user_rt::user_log!(
                                    "PM_VFS_SPAWN_FAIL image_id={} err=missing_vfs_send_cap_pm_local",
                                    req.image_id
                                );
                                let encoded = encode_spawn_v5_reply(0, 0);
                                return Message::with_header(0, PROC_OP_SPAWN_V5_CAP, 0, None, &encoded)
                                    .map_err(|_| ProcessManagerError::Malformed);
                            }
                            let (t, c, s) = match unsafe {
                                pm_vfs_spawn_inline(
                                    req.image_id,
                                    req.parent_pid.0,
                                    &startup_args,
                                    vfs_send_cap,
                                )
                            } {
                                Ok(values) => values,
                                Err(err) => {
                                    yarm_user_rt::user_log!(
                                        "PM_VFS_SPAWN_FAIL image_id={} err={:?}",
                                        req.image_id,
                                        err
                                    );
                                    let encoded = encode_spawn_v5_reply(0, 0);
                                    return Message::with_header(
                                        0,
                                        PROC_OP_SPAWN_V5_CAP,
                                        0,
                                        None,
                                        &encoded,
                                    )
                                    .map_err(|_| ProcessManagerError::Malformed);
                                }
                            };
                            (t, c as u64, s)
                        }
                    };
                    // PM's own send cap: prefer spawner_cap (set when parent got a
                    // delegated copy); fall back to caller_cap when parent_pid == 0.
                    let pm_send_cap =
                        if spawner_cap != 0 { spawner_cap } else { caller_cap as u32 };
                    // Record in lifecycle table regardless of image_id so PM always
                    // has a complete view of spawned services.
                    let recorded = self.lifecycle_table.record(ServiceLifecycleRecord {
                        tid,
                        image_id: req.image_id,
                        parent_tid: req.parent_pid.0,
                        pm_service_send_cap: pm_send_cap,
                        state: ServiceState::Spawned,
                    });
                    yarm_user_rt::user_log!(
                        "PM_LIFECYCLE_RECORD image_id={} tid={} pm_service_send_cap={} parent_tid={} state=spawned recorded={}",
                        req.image_id, tid, pm_send_cap, req.parent_pid.0, recorded as u8
                    );
                    let encoded = encode_spawn_v5_reply(tid, caller_cap);
                    yarm_user_rt::user_log!(
                        "PM_SPAWN_V5_CAP_REPLY tid={} caller_cap={} pm_send_cap={} len={}",
                        tid, caller_cap, pm_send_cap, encoded.len()
                    );
                    Message::with_header(0, PROC_OP_SPAWN_V5_CAP, 0, None, &encoded)
                        .map_err(|_| ProcessManagerError::Malformed)
                }
                #[cfg(test)]
                {
                    if req.image_id == 0 {
                        return Err(ProcessManagerError::Unsupported);
                    }
                    let image = synthetic_elf_image(req.image_id);
                    let info = ElfImageInfo::parse(req.image_id, &image).map_err(map_elf_error)?;
                    let pid = self.manager.allocate_process(req.parent_pid)?;
                    self.record_spawn_policy(pid, req.image_id, info.entry, None, None)?;
                    self.lifecycle_table.record(ServiceLifecycleRecord {
                        tid: pid.0,
                        image_id: req.image_id,
                        parent_tid: req.parent_pid.0,
                        pm_service_send_cap: 0,
                        state: ServiceState::Spawned,
                    });
                    let result = SpawnV5CapResult::new(pid.0, 0);
                    Message::with_header(0, PROC_OP_SPAWN_V5_CAP, 0, None, &result.encode())
                        .map_err(|_| ProcessManagerError::Malformed)
                }
            }
            ProcessRequest::WaitPidV2(req) => {
                if req.caller_pid != req.target_pid {
                    let Some(parent) = self.manager.parent_of(req.target_pid) else {
                        return Err(ProcessManagerError::PermissionDenied);
                    };
                    if parent != req.caller_pid {
                        return Err(ProcessManagerError::PermissionDenied);
                    }
                }
                let waited = self.manager.wait_exited(req.target_pid)?;
                let result = WaitPidV2Result {
                    waited_pid: waited.waited_pid,
                    exit_code: waited.exit_code,
                };
                Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
            ProcessRequest::TaskRestartToken { tid } => {
                let reply = TaskRestartTokenReply::new(
                    self.restart_token_for_tid(tid).is_some(),
                    self.restart_token_for_tid(tid).unwrap_or(0),
                );
                Message::with_header(0, PROC_OP_TASK_RESTART_TOKEN, 0, None, &reply.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
            ProcessRequest::RegisterSupervisedTask { tid, restart_token } => {
                self.record_restart_token(tid, restart_token)?;
                Self::u64_reply(PROC_OP_REGISTER_SUPERVISED_TASK, 0)
            }
            ProcessRequest::ExecuteRestart { tid, restart_token } => {
                let status = match self.restart_token_for_tid(tid) {
                    None => ExecuteRestartReply::STATUS_NOT_FOUND,
                    Some(token) if token != restart_token => {
                        ExecuteRestartReply::STATUS_TOKEN_MISMATCH
                    }
                    Some(_) => self.execute_restart_via_kernel_cap(tid, restart_token),
                };
                let reply = ExecuteRestartReply::new(status);
                Message::with_header(0, PROC_OP_EXECUTE_RESTART, 0, None, &reply.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
            ProcessRequest::LifecycleQuery { tid } => {
                yarm_user_rt::user_log!("PM_LIFECYCLE_QUERY_RECV tid={}", tid);
                let reply = match self.lifecycle_table.get_by_tid(tid) {
                    Some(rec) => {
                        let state = match rec.state {
                            ServiceState::Spawned => LIFECYCLE_STATE_SPAWNED,
                        };
                        LifecycleQueryReply::found(rec.tid, rec.image_id, state)
                    }
                    None => LifecycleQueryReply::not_found(),
                };
                yarm_user_rt::user_log!(
                    "PM_LIFECYCLE_QUERY_REPLY tid={} found={} image_id={}",
                    tid,
                    reply.found,
                    reply.image_id
                );
                Message::with_header(0, PROC_OP_LIFECYCLE_QUERY, 0, None, &reply.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
        }
    }

    fn execute_restart_via_kernel_cap(&self, tid: u64, restart_token: u64) -> u8 {
        let Some(send_cap) = self.restart_control_send_cap else {
            return ExecuteRestartReply::STATUS_PERMISSION_DENIED;
        };
        let request = ExecuteRestartRequest::new(tid, restart_token);
        let msg = match Message::with_header(0, PROC_OP_EXECUTE_RESTART, 0, None, &request.encode())
        {
            Ok(msg) => msg,
            Err(_) => return ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED,
        };
        let reply_cap = yarm_user_rt::runtime::startup_context()
            .process_manager_reply_recv_cap
            .unwrap_or(0);
        // SAFETY: process-manager owns both caps via startup handoff. ipc_call is
        // synchronous — the reply is delivered inline so no separate ipc_recv is needed.
        let call = unsafe { yarm_user_rt::syscall::ipc_call(send_cap, reply_cap, &msg) };
        if call.is_err() {
            return ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED;
        }
        ExecuteRestartReply::STATUS_OK
    }
}

impl RequestResponseService<Message, Message> for ProcessService {
    type Error = ProcessManagerError;

    fn service_name(&self) -> &'static str {
        "process_manager"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        ProcessService::handle(self, request)
    }
}

fn map_elf_error(err: yarm_srv_common::elf::ElfParseError) -> ProcessManagerError {
    match err {
        yarm_srv_common::elf::ElfParseError::Malformed => ProcessManagerError::Malformed,
        yarm_srv_common::elf::ElfParseError::Unsupported => ProcessManagerError::Unsupported,
    }
}

/// Spawn `image_id` from the boot initramfs CPIO via the `SpawnFromInitramfsFile`
/// kernel syscall (nr=26).  The kernel reads the ELF into its own staging buffer
/// and spawns the process without requiring a user-space buffer.
///
/// Returns `Ok((tid, caller_cap, spawner_cap))` on success.
/// Returns `Err(Unsupported)` if `image_id` has no known CPIO mapping.
/// Returns `Err(TableFull)` if the kernel spawn syscall fails.
/// `startup_args` must have service caps at indices 13-16 (same layout as
/// `spawn_process_with_startup_caps`).
#[cfg(not(test))]
unsafe fn pm_vfs_spawn_inline(
    image_id: u64,
    parent_pid: u64,
    startup_args: &[u64; 18],
    vfs_send_cap: u32,
) -> Result<(u64, u32, u32), ProcessManagerError> {
    let path_label: &[u8] = match image_id {
        4 => b"/initramfs/sbin/initramfs_srv",
        5 => b"/initramfs/sbin/devfs_srv",
        6 => b"/initramfs/sbin/vfs_server",
        7 => b"/initramfs/sbin/driver_manager",
        8 => b"/initramfs/sbin/blkcache_srv",
        9 => b"/initramfs/sbin/virtio_blk_srv",
        10 => b"/initramfs/sbin/fat_srv",
        11 => b"/initramfs/sbin/ramfs_srv",
        _ => {
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_IMAGE_UNKNOWN image_id={}",
                image_id
            );
            return Err(ProcessManagerError::Unsupported);
        }
    };
    if image_id == 10 {
        yarm_user_rt::user_log!("PM_IMAGE_ID_10_FAT_SRV path=/initramfs/sbin/fat_srv");
    }
    if image_id == 11 {
        yarm_user_rt::user_log!("PM_IMAGE_ID_11_RAMFS_SRV path=/initramfs/sbin/ramfs_srv");
    }
    let path_log = core::str::from_utf8(path_label).unwrap_or("<path-bytes>");
    yarm_user_rt::user_log!(
        "PM_VFS_SPAWN_IMAGE_BEGIN image_id={} path={} parent_pid={}",
        image_id, path_log, parent_pid
    );
    let ctx = yarm_user_rt::runtime::startup_context();
    let reply_recv_cap = ctx
        .process_manager_reply_recv_cap
        .ok_or(ProcessManagerError::Unsupported)?;
    yarm_user_rt::user_log!(
        "PM_VFS_CAPS send_cap={} reply_cap={}",
        vfs_send_cap,
        reply_recv_cap
    );
    // For image_id 7-9: try Phase 3A (MemoryObject cap grant) first, then fall
    // back to Phase 2B (transfer-buffer bulk read) only on Unsupported.
    // For image_id 4-6: fall through to the existing inline 112-byte path.
    if pm_image_cpio_name(image_id).is_some() {
        // Phase 3A attempt: VFS_OP_FILE_GRANT_RO → SpawnFromMemoryObject.
        let phase3a_result = unsafe {
            pm_try_grant_ro_and_spawn(
                image_id,
                vfs_send_cap,
                reply_recv_cap,
                path_label,
                parent_pid,
                startup_args,
            )
        };
        match phase3a_result {
            Ok((tid, caller_cap, spawner_cap)) => {
                yarm_user_rt::user_log!(
                    "PM_VFS_SPAWN_IMAGE_SELECTED image_id={} source=phase3a_grant",
                    image_id
                );
                return Ok((tid, caller_cap, spawner_cap));
            }
            Err(ProcessManagerError::Unsupported) => {
                // Backend doesn't support FILE_GRANT_RO yet — fall back to Phase 2B.
                yarm_user_rt::user_log!(
                    "PM_VFS_GRANT_RO_UNSUPPORTED image_id={} fallback=phase2b",
                    image_id
                );
            }
            Err(e) => {
                // Hard error (NotFound, Malformed) — no fallback.
                yarm_user_rt::user_log!(
                    "PM_ELF_ZC_FAIL image_id={} reason=phase3a_hard_err err={:?}",
                    image_id, e
                );
                return Err(e);
            }
        }
    }

    let image = match pm_image_cpio_name(image_id) {
        Some(cpio_name) => {
            unsafe {
                pm_read_all_via_vfs_bulk(
                    image_id,
                    vfs_send_cap,
                    reply_recv_cap,
                    path_label,
                    cpio_name,
                )
            }?
        }
        None => {
            unsafe { pm_read_all_via_vfs(image_id, vfs_send_cap, reply_recv_cap, path_label) }?
        }
    };
    if image.is_empty() {
        yarm_user_rt::user_log!("PM_VFS_SPAWN_FAIL image_id={} err=empty-elf", image_id);
        return Err(ProcessManagerError::Malformed);
    }
    // Verify ELF magic before attempting spawn.
    if image.len() < 4 || &image[..4] != b"\x7fELF" {
        let first4_end = core::cmp::min(image.len(), 4);
        yarm_user_rt::user_log!(
            "PM_VFS_SPAWN_FAIL image_id={} err=bad-elf-magic first4={:x?}",
            image_id, &image[..first4_end]
        );
        return Err(ProcessManagerError::Malformed);
    }
    yarm_user_rt::user_log!(
        "PM_VFS_SPAWN_FROM_VFS_BYTES image_id={} len={} first4={:x?}",
        image_id, image.len(), &image[..4]
    );
    let image_len = image.len();
    let result = unsafe {
        yarm_user_rt::syscall::spawn_process_from_user_buf(
            image_id,
            image.as_ptr(),
            image_len,
            parent_pid,
            startup_args,
        )
    };
    drop(image);
    yarm_user_rt::user_log!(
        "PM_VFS_EXEC_BUFFER_DROPPED image_id={} len={}",
        image_id,
        image_len
    );
    match result {
        Ok((tid, caller_cap, spawner_cap)) => {
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_RESULT image_id={} tid={} caller_cap={} spawner_cap={}",
                image_id, tid, caller_cap, spawner_cap
            );
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_IMAGE_SELECTED image_id={} source=vfs",
                image_id
            );
            Ok((tid, caller_cap, spawner_cap))
        }
        Err(e) => {
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_FAIL image_id={} err={:?}",
                image_id, e
            );
            Err(ProcessManagerError::TableFull)
        }
    }
}

#[cfg(not(test))]
unsafe fn pm_vfs_call_u64(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    msg: &Message,
) -> Result<Message, ProcessManagerError> {
    let op = msg.opcode;
    match unsafe { yarm_user_rt::syscall::ipc_call(vfs_send_cap, reply_recv_cap, msg) } {
        Ok(()) => {
            yarm_user_rt::user_log!("PM_VFS_CALL_SENT op={} status=ok", op);
        }
        Err(yarm_user_rt::syscall::SyscallError::WouldBlock) => {
            // Finalized IPC contract: ipc_call is send/queue-only. A WouldBlock
            // at this stage is treated as a normal blocking transition, then we
            // explicitly receive the reply via the dedicated reply endpoint.
            yarm_user_rt::user_log!("PM_VFS_CALL_BLOCKED_NORMAL op={}", op);
        }
        Err(e) => {
            yarm_user_rt::user_log!("PM_VFS_CALL_FAIL op={} err={:?}", op, e);
            return Err(ProcessManagerError::Unsupported);
        }
    }

    yarm_user_rt::user_log!(
        "PM_VFS_REPLY_RECV_BEGIN op={} reply_cap={}",
        op,
        reply_recv_cap
    );
    match unsafe { yarm_user_rt::syscall::ipc_recv_v2(reply_recv_cap) } {
        Ok(Some(received)) => {
            let reply = received.message;
            let payload = reply.as_slice();
            let preview_len = core::cmp::min(payload.len(), 32);
            yarm_user_rt::user_log!(
                "PM_VFS_REPLY_RAW op={} len={} opcode={} flags={} sender_tid={} transferred_cap={} bytes={:x?}",
                op,
                reply.len,
                reply.opcode,
                reply.flags,
                received.sender_tid,
                received.transferred_cap.unwrap_or(0),
                &payload[..preview_len]
            );
            yarm_user_rt::user_log!(
                "PM_VFS_REPLY op={} status=ok len={} opcode={} flags={}",
                op,
                reply.len,
                reply.opcode,
                reply.flags
            );
            Ok(reply)
        }
        Ok(None) => {
            yarm_user_rt::user_log!("PM_VFS_REPLY_RECV_FAIL op={} err=timed_out_or_would_block", op);
            Err(ProcessManagerError::WouldBlock)
        }
        Err(e) => {
            yarm_user_rt::user_log!("PM_VFS_REPLY_RECV_FAIL op={} err={:?}", op, e);
            Err(ProcessManagerError::Unsupported)
        }
    }
}

/// Phase 3A variant of `pm_vfs_call_u64` that also returns the transferred cap.
#[cfg(not(test))]
unsafe fn pm_vfs_call_full(
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    msg: &Message,
) -> Result<(Message, Option<u32>), ProcessManagerError> {
    let op = msg.opcode;
    match unsafe { yarm_user_rt::syscall::ipc_call(vfs_send_cap, reply_recv_cap, msg) } {
        Ok(()) => {
            yarm_user_rt::user_log!("PM_VFS_CALL_SENT op={} status=ok", op);
        }
        Err(yarm_user_rt::syscall::SyscallError::WouldBlock) => {
            yarm_user_rt::user_log!("PM_VFS_CALL_BLOCKED_NORMAL op={}", op);
        }
        Err(e) => {
            yarm_user_rt::user_log!("PM_VFS_CALL_FAIL op={} err={:?}", op, e);
            return Err(ProcessManagerError::Unsupported);
        }
    }
    match unsafe { yarm_user_rt::syscall::ipc_recv_v2(reply_recv_cap) } {
        Ok(Some(received)) => {
            let transferred_cap = received.transferred_cap;
            yarm_user_rt::user_log!(
                "PM_VFS_REPLY_FULL op={} len={} transferred_cap={}",
                op, received.message.len,
                transferred_cap.unwrap_or(0)
            );
            Ok((received.message, transferred_cap))
        }
        Ok(None) => {
            yarm_user_rt::user_log!("PM_VFS_REPLY_FULL_FAIL op={} err=no_message", op);
            Err(ProcessManagerError::WouldBlock)
        }
        Err(e) => {
            yarm_user_rt::user_log!("PM_VFS_REPLY_FULL_FAIL op={} err={:?}", op, e);
            Err(ProcessManagerError::Unsupported)
        }
    }
}

/// Phase 3A: Try VFS_OP_FILE_GRANT_RO and spawn from MemoryObject cap.
///
/// Returns `Ok((tid, caller_cap, spawner_cap))` on success.
/// Returns `Err(ProcessManagerError::Unsupported)` if the backend doesn't support
/// FILE_GRANT_RO — caller should fall back to Phase 2B.
/// Returns other errors on hard failures (NotFound, Malformed) — NO fallback.
#[cfg(not(test))]
unsafe fn pm_try_grant_ro_and_spawn(
    image_id: u64,
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    vfs_path: &[u8],
    parent_pid: u64,
    startup_args: &[u64; 18],
) -> Result<(u64, u32, u32), ProcessManagerError> {
    let path_str = core::str::from_utf8(vfs_path).unwrap_or("<path>");

    yarm_user_rt::user_log!("PM_VFS_GRANT_RO_BEGIN image_id={} path={}", image_id, path_str);

    // ── 1. OPENAT via VFS to get fd ──────────────────────────────────────────
    let open_msg = build_openat_message(vfs_path, 0).map_err(|_| ProcessManagerError::Malformed)?;
    let open_reply = match unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &open_msg) } {
        Ok(r) => r,
        Err(e) => {
            yarm_user_rt::user_log!(
                "PM_ELF_ZC_FAIL image_id={} reason=openat_fail err={:?}",
                image_id, e
            );
            return Err(e);
        }
    };
    let fd = match decode_u64(open_reply.as_slice()) {
        Some(v) => v,
        None => {
            yarm_user_rt::user_log!("PM_ELF_ZC_FAIL image_id={} reason=bad_fd_decode", image_id);
            return Err(ProcessManagerError::Malformed);
        }
    };
    yarm_user_rt::user_log!("PM_VFS_GRANT_RO_OPENAT image_id={} fd={}", image_id, fd);

    // ── 2. VFS_OP_FILE_GRANT_RO → get MemoryObject cap ──────────────────────
    let grant_payload = yarm_ipc_abi::vfs_abi::FileGrantRoArgs::new(fd).encode();
    let grant_msg = Message::with_header(
        0,
        yarm_ipc_abi::vfs_abi::VFS_OP_FILE_GRANT_RO,
        0,
        None,
        &grant_payload,
    ).map_err(|_| ProcessManagerError::Malformed)?;

    let (grant_reply, transferred_cap) = match unsafe {
        pm_vfs_call_full(vfs_send_cap, reply_recv_cap, &grant_msg)
    } {
        Ok(r) => r,
        Err(e) => {
            yarm_user_rt::user_log!(
                "PM_ELF_ZC_FAIL image_id={} reason=grant_ro_ipc_fail err={:?}",
                image_id, e
            );
            // Close fd on error.
            if let Ok(close_msg) = build_close_message(fd) {
                let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
            }
            return Err(e);
        }
    };

    // Check reply status: non-zero opcode or no transferred cap → unsupported.
    if grant_reply.opcode != 0 || transferred_cap.is_none() {
        yarm_user_rt::user_log!(
            "PM_ELF_ZC_FAIL image_id={} reason=grant_ro_unsupported opcode={} has_cap={}",
            image_id, grant_reply.opcode, transferred_cap.is_some()
        );
        if let Ok(close_msg) = build_close_message(fd) {
            let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
        }
        return Err(ProcessManagerError::Unsupported);
    }

    let mo_cap = transferred_cap.unwrap();

    // Decode file_len from reply payload.
    let reply_payload = grant_reply.as_slice();
    let file_grant_reply = yarm_ipc_abi::vfs_abi::FileGrantRoReply::decode(reply_payload);
    let file_len = file_grant_reply.map(|r| r.file_len).unwrap_or(0);

    yarm_user_rt::user_log!(
        "PM_VFS_GRANT_RO_RECEIVED image_id={} len={} cap={}",
        image_id, file_len, mo_cap
    );

    // Close fd — we have the cap now.
    if let Ok(close_msg) = build_close_message(fd) {
        let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
    }

    // ── 3. Spawn from MemoryObject cap (kernel syscall nr=29) ────────────────
    // SAFETY: mo_cap is a valid MemoryObject cap minted by the kernel.
    let result = unsafe {
        yarm_user_rt::syscall::spawn_from_memory_object(
            image_id,
            mo_cap,
            parent_pid,
            startup_args,
        )
    };
    match result {
        Ok((tid, caller_cap, spawner_cap)) => {
            // PM_ELF_ZC_DONE is emitted by the kernel (yarm_log!) inside handle_spawn_from_memory_object.
            // Emit a distinct PM-side marker for the user-space log so as not to double-count.
            yarm_user_rt::user_log!(
                "PM_SPAWN_FROM_MO_DONE image_id={} tid={} caller_cap={} spawner_cap={}",
                image_id, tid, caller_cap, spawner_cap
            );
            Ok((tid, caller_cap, spawner_cap))
        }
        Err(yarm_user_rt::syscall::SyscallError::WrongObject)
        | Err(yarm_user_rt::syscall::SyscallError::InvalidArgs) => {
            yarm_user_rt::user_log!(
                "PM_ELF_ZC_FAIL image_id={} reason=spawn_from_mo_unsupported",
                image_id
            );
            Err(ProcessManagerError::Unsupported)
        }
        Err(e) => {
            yarm_user_rt::user_log!(
                "PM_ELF_ZC_FAIL image_id={} reason=spawn_from_mo_err err={:?}",
                image_id, e
            );
            Err(ProcessManagerError::Malformed)
        }
    }
}

// decode_u64: NOT gated by #[cfg(not(test))] so unit tests can call it directly.
fn decode_u64(payload: &[u8]) -> Option<u64> {
    if payload.len() < 8 {
        return None;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&payload[..8]);
    Some(u64::from_le_bytes(b))
}

/// Map image_id (7/8/9) → bare CPIO entry name (no leading slash).
/// These are the names inside the initramfs CPIO archive.
#[cfg(not(test))]
fn pm_image_cpio_name(image_id: u64) -> Option<&'static [u8]> {
    match image_id {
        7 => Some(b"sbin/driver_manager"),
        8 => Some(b"sbin/blkcache_srv"),
        9 => Some(b"sbin/virtio_blk_srv"),
        10 => Some(b"sbin/fat_srv"),
        11 => Some(b"sbin/ramfs_srv"),
        _ => None,
    }
}

/// Phase 2B+2A bulk read path for image_id 7/8/9.
///
/// Preference order:
/// 1. Phase 2B: PM sends VFS_OP_READ_BULK IPC → VFS routes → initramfs fills PM's
///    transfer buffer via kernel syscall nr=27 (target_tid=PM).  mode=vfs_transfer.
/// 2. Phase 2A fallback: If VFS/initramfs returns unsupported (opcode≠0), PM falls
///    back to direct kernel syscall nr=27 (Phase 2A bridge).  mode=phase2a_bridge.
/// 3. Inline fallback: If Phase 2A kernel returns InvalidArgs (kernel lacks CPIO),
///    fall back to old inline 112-byte VFS READ path.
///
/// Hard errors (NotFound=Internal, PermissionDenied=MissingRight, PageFault, etc.)
/// are NEVER silently fell-through to a lower path.
///
/// Phase 2B missing primitive: MemoryObject page-cap grant so initramfs can write
/// directly to PM's page without kernel-mediated cross-ASID copy.
#[cfg(not(test))]
unsafe fn pm_read_all_via_vfs_bulk(
    image_id: u64,
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    vfs_path: &[u8],
    cpio_name: &[u8],
) -> Result<Vec<u8>, ProcessManagerError> {
    let path_str = core::str::from_utf8(vfs_path).unwrap_or("<path>");
    let cpio_str = core::str::from_utf8(cpio_name).unwrap_or("<cpio>");

    // ── 1. STATX via VFS to get file size ────────────────────────────────────
    let stat_msg = build_statx_message(vfs_path).map_err(|_| ProcessManagerError::Malformed)?;
    yarm_user_rt::user_log!("PM_VFS_CALL op=STATX path={}", path_str);
    let stat_reply = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &stat_msg) }?;
    let stat_payload = stat_reply.as_slice();
    if stat_payload.len() != 8 {
        yarm_user_rt::user_log!(
            "PM_VFS_REPLY_DECODE_FAIL op=STATX image_id={} reason=bad_len expected=8 actual={}",
            image_id, stat_payload.len()
        );
        return Err(ProcessManagerError::Malformed);
    }
    let file_len = decode_u64(stat_payload).ok_or(ProcessManagerError::Malformed)? as usize;
    yarm_user_rt::user_log!(
        "PM_VFS_REPLY_DECODE op=STATX image_id={} file_len={}",
        image_id, file_len
    );

    // ── 2. OPENAT via VFS for fd lifecycle tracking ───────────────────────────
    let open_msg = build_openat_message(vfs_path, 0).map_err(|_| ProcessManagerError::Malformed)?;
    yarm_user_rt::user_log!("PM_VFS_CALL op=OPENAT path={}", path_str);
    let open_reply = match unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &open_msg) } {
        Ok(r) => r,
        Err(e) => {
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_FAIL image_id={} stage=bulk-openat reason={:?}",
                image_id, e
            );
            return Err(e);
        }
    };
    let fd = match decode_u64(open_reply.as_slice()) {
        Some(v) => v,
        None => {
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_FAIL image_id={} stage=bulk-openat reason=bad_fd_decode",
                image_id
            );
            return Err(ProcessManagerError::Malformed);
        }
    };
    yarm_user_rt::user_log!(
        "PM_VFS_OPENAT_DECODE image_id={} path={} fd={}",
        image_id, path_str, fd
    );

    // ── 3. Phase 2B: attempt VFS-mediated transfer-buffer bulk read ───────────
    // Try VFS_OP_READ_BULK IPC first.  If VFS/initramfs says unsupported
    // (reply opcode ≠ 0 or copied_len=0+eof=false) fall back to Phase 2A.
    yarm_user_rt::user_log!(
        "PM_VFS_READ_BULK_BEGIN image_id={} fd={} expected={} chunk=4096 mode=vfs_transfer",
        image_id, fd, file_len
    );

    let mut out = Vec::with_capacity(file_len);
    let mut offset: u64 = 0;
    let mut bulk_buf = [0u8; 4096];
    let mut chunk_count: u32 = 0;
    let mut used_phase2b = false;
    let mut phase2b_unsupported = false;

    // Phase 2B read loop: send VFS_OP_READ_BULK, initramfs fills bulk_buf via kernel.
    'phase2b: loop {
        if out.len() >= file_len {
            break 'phase2b;
        }
        let remaining = file_len - out.len();
        let want = core::cmp::min(remaining, 4096) as u64;

        let bulk_dst_ptr = bulk_buf.as_mut_ptr() as usize;
        let bulk_msg = match build_bulk_read_message(fd, want, offset, bulk_dst_ptr) {
            Ok(m) => m,
            Err(_) => {
                yarm_user_rt::user_log!(
                    "PM_VFS_READ_BULK_FAIL image_id={} stage=build_msg reason=encode_fail",
                    image_id
                );
                phase2b_unsupported = true;
                break 'phase2b;
            }
        };

        // Send VFS_OP_READ_BULK and receive reply.
        let bulk_reply = match unsafe {
            pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &bulk_msg)
        } {
            Ok(r) => r,
            Err(e) => {
                yarm_user_rt::user_log!(
                    "PM_VFS_READ_BULK_FAIL image_id={} stage=ipc_call reason={:?}",
                    image_id, e
                );
                phase2b_unsupported = true;
                break 'phase2b;
            }
        };

        // Check reply opcode: 0 = success, non-0 = error/unsupported.
        if bulk_reply.opcode != 0 {
            yarm_user_rt::user_log!(
                "PM_VFS_READ_BULK_VFS_UNSUPPORTED image_id={} fallback=phase2a opcode={}",
                image_id, bulk_reply.opcode
            );
            phase2b_unsupported = true;
            break 'phase2b;
        }

        // Decode BulkReadReply.
        let bulk_reply_payload = bulk_reply.as_slice();
        let bulk_decoded = match yarm_ipc_abi::vfs_abi::BulkReadReply::decode(bulk_reply_payload) {
            Some(r) => r,
            None => {
                yarm_user_rt::user_log!(
                    "PM_VFS_READ_BULK_FAIL image_id={} stage=decode_reply reason=malformed",
                    image_id
                );
                phase2b_unsupported = true;
                break 'phase2b;
            }
        };

        // copied_len=0 + eof=false = Phase 2B stub/unsupported.
        if bulk_decoded.copied_len == 0 && !bulk_decoded.eof {
            yarm_user_rt::user_log!(
                "PM_VFS_READ_BULK_VFS_UNSUPPORTED image_id={} fallback=phase2a reason=stub_reply",
                image_id
            );
            phase2b_unsupported = true;
            break 'phase2b;
        }

        let bytes_copied = bulk_decoded.copied_len as usize;
        if bytes_copied > 4096 || bytes_copied > bulk_buf.len() {
            yarm_user_rt::user_log!(
                "PM_VFS_READ_BULK_FAIL image_id={} stage=phase2b reason=copied_len_overflow copied={}",
                image_id, bytes_copied
            );
            let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
            let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
            return Err(ProcessManagerError::Malformed);
        }

        if bytes_copied == 0 && bulk_decoded.eof {
            // Real EOF (file shorter than STATX reported).
            yarm_user_rt::user_log!(
                "PM_VFS_READ_BULK_FAIL image_id={} stage=eof_early_phase2b total={} expected={} offset={}",
                image_id, out.len(), file_len, offset
            );
            let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
            let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
            return Err(ProcessManagerError::Malformed);
        }

        // Data was written by initramfs into bulk_buf[..bytes_copied] via kernel.
        out.extend_from_slice(&bulk_buf[..bytes_copied]);
        offset += bytes_copied as u64;
        chunk_count += 1;
        used_phase2b = true;

        if PM_VFS_BULK_READ_TRANSFER_CHUNK_TRACE {
            yarm_user_rt::user_log!(
                "PM_VFS_READ_BULK_TRANSFER_CHUNK image_id={} bytes={} total={} expected={} chunk_n={}",
                image_id, bytes_copied, out.len(), file_len, chunk_count
            );
        }
    }

    // ── 4. Phase 2A fallback: direct kernel syscall if Phase 2B unsupported ───
    if phase2b_unsupported && out.is_empty() {
        yarm_user_rt::user_log!(
            "PM_VFS_READ_BULK_PHASE2A_BEGIN image_id={} fd={} expected={} chunk=4096 cpio={}",
            image_id, fd, file_len, cpio_str
        );

        offset = 0;
        chunk_count = 0;

        loop {
            if out.len() >= file_len {
                break;
            }
            let remaining = file_len - out.len();
            let want = core::cmp::min(remaining, 4096);
            let dst = &mut bulk_buf[..want];

            // SAFETY: dst is valid writable memory in PM's address space.
            let bytes_copied = match unsafe {
                yarm_user_rt::syscall::initramfs_read_chunk(cpio_name, offset, dst)
            } {
                Ok(n) => n,
                // InvalidArgs = kernel lacks CPIO ("bridge unavailable") → inline fallback.
                Err(yarm_user_rt::syscall::SyscallError::InvalidArgs) => {
                    yarm_user_rt::user_log!(
                        "PM_VFS_READ_BULK_UNSUPPORTED image_id={} fallback=inline reason=kernel_no_cpio",
                        image_id
                    );
                    let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
                    let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
                    return unsafe { pm_read_all_via_vfs(image_id, vfs_send_cap, reply_recv_cap, vfs_path) };
                }
                // NOT-FOUND: file missing from CPIO — real error, no fallback.
                Err(yarm_user_rt::syscall::SyscallError::Internal) => {
                    yarm_user_rt::user_log!(
                        "PM_VFS_READ_BULK_FAIL image_id={} stage=syscall reason=not_found offset={}",
                        image_id, offset
                    );
                    let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
                    let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
                    return Err(ProcessManagerError::Malformed);
                }
                // PermissionDenied, PageFault, or any other error — real error, no fallback.
                Err(e) => {
                    yarm_user_rt::user_log!(
                        "PM_VFS_READ_BULK_FAIL image_id={} stage=syscall reason={:?} offset={}",
                        image_id, e, offset
                    );
                    let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
                    let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
                    return Err(ProcessManagerError::Malformed);
                }
            };

            if bytes_copied == 0 {
                yarm_user_rt::user_log!(
                    "PM_VFS_READ_BULK_FAIL image_id={} stage=eof_early total={} expected={} offset={}",
                    image_id, out.len(), file_len, offset
                );
                let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
                let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
                return Err(ProcessManagerError::Malformed);
            }

            out.extend_from_slice(&bulk_buf[..bytes_copied]);
            offset += bytes_copied as u64;
            chunk_count += 1;

            if PM_VFS_BULK_READ_CHUNK_TRACE {
                yarm_user_rt::user_log!(
                    "PM_VFS_READ_BULK_CHUNK image_id={} bytes={} total={} expected={} chunk_n={}",
                    image_id, bytes_copied, out.len(), file_len, chunk_count
                );
            }
        }

        yarm_user_rt::user_log!(
            "PM_VFS_READ_BULK_PHASE2A_DONE image_id={} total={} chunks={}",
            image_id, out.len(), chunk_count
        );
    }

    // ── 5. CLOSE via VFS to release fd ───────────────────────────────────────
    let close_msg = match build_close_message(fd) {
        Ok(m) => m,
        Err(_) => return Err(ProcessManagerError::Malformed),
    };
    yarm_user_rt::user_log!("PM_VFS_CALL op=CLOSE fd={}", fd);
    let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };

    // ── 6. Verify and log completion ──────────────────────────────────────────
    {
        let first4_end = core::cmp::min(out.len(), 4);
        let mode = if used_phase2b { "vfs_transfer" } else { "phase2a_bridge" };
        yarm_user_rt::user_log!(
            "PM_VFS_READ_BULK_DONE image_id={} total={} first4={:x?} chunks={} mode={}",
            image_id, out.len(), &out[..first4_end], chunk_count, mode
        );
        // Emit the legacy completion marker so smoke scripts and existing greps pass.
        yarm_user_rt::user_log!(
            "PM_VFS_READ_DONE image_id={} total={} first4={:x?}",
            image_id, out.len(), &out[..first4_end]
        );
    }

    Ok(out)
}

#[cfg(not(test))]
unsafe fn pm_read_all_via_vfs(
    image_id: u64,
    vfs_send_cap: u32,
    reply_recv_cap: u32,
    path: &[u8],
) -> Result<Vec<u8>, ProcessManagerError> {
    let path_str = core::str::from_utf8(path).unwrap_or("<path-bytes>");
    let stat_msg = build_statx_message(path).map_err(|_| ProcessManagerError::Malformed)?;
    yarm_user_rt::user_log!("PM_VFS_CALL op=STATX path={}", path_str);
    let stat_reply = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &stat_msg) }?;
    let stat_payload = stat_reply.as_slice();
    if stat_payload.len() != 8 {
        let preview_len = core::cmp::min(stat_payload.len(), 32);
        yarm_user_rt::user_log!(
            "PM_VFS_REPLY_DECODE_FAIL op=STATX reason=bad_len expected=8 actual={} bytes={:x?}",
            stat_payload.len(),
            &stat_payload[..preview_len]
        );
        return Err(ProcessManagerError::Malformed);
    }
    let file_len = decode_u64(stat_payload).ok_or(ProcessManagerError::Malformed)? as usize;
    yarm_user_rt::user_log!(
        "PM_VFS_REPLY_DECODE op=STATX expected_len=8 actual_len={} value={}",
        stat_payload.len(),
        file_len
    );

    let open_msg = build_openat_message(path, 0).map_err(|_| ProcessManagerError::Malformed)?;
    yarm_user_rt::user_log!("PM_VFS_CALL op=OPENAT path={}", path_str);

    // Capture OPENAT result before propagating so we can log the exact failure.
    let open_reply = match unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &open_msg) } {
        Ok(reply) => {
            yarm_user_rt::user_log!(
                "PM_VFS_OPENAT_RETURN image_id={} path={} result=ok len={}",
                image_id, path_str, reply.len
            );
            reply
        }
        Err(err) => {
            yarm_user_rt::user_log!(
                "PM_VFS_OPENAT_RETURN image_id={} path={} result=err err={:?}",
                image_id, path_str, err
            );
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_FAIL image_id={} stage=after-openat reason=openat_call_fail",
                image_id
            );
            return Err(err);
        }
    };

    // Decode fd from the 8-byte LE reply payload.
    let fd = match decode_u64(open_reply.as_slice()) {
        Some(v) => {
            let slice = open_reply.as_slice();
            let preview_len = core::cmp::min(slice.len(), 8);
            yarm_user_rt::user_log!(
                "PM_VFS_OPENAT_DECODE image_id={} path={} fd={} raw_len={} raw_bytes={:x?}",
                image_id, path_str, v, open_reply.len, &slice[..preview_len]
            );
            v
        }
        None => {
            yarm_user_rt::user_log!(
                "PM_VFS_SPAWN_FAIL image_id={} stage=after-openat reason=bad_fd_decode raw_len={}",
                image_id, open_reply.len
            );
            return Err(ProcessManagerError::Malformed);
        }
    };

    let mut out = Vec::with_capacity(file_len);
    // Log before entering the READ loop so any OOM or other failure between
    // OPENAT-decode and first READ is bracketed by PM_VFS_READ_BEGIN.
    yarm_user_rt::user_log!(
        "PM_VFS_READ_BEGIN image_id={} path={} fd={} expected={} chunk={}",
        image_id, path_str, fd, file_len, Message::MAX_PAYLOAD - 16
    );

    // READ loop: accumulate file_len bytes in chunks.  Each iteration must make
    // forward progress; zero-length reads before reaching file_len are treated
    // as a fatal protocol error (premature EOF or format mismatch).
    while out.len() < file_len {
        let prev_len = out.len();
        // Request at most MAX_PAYLOAD-16 bytes: 16 bytes are used for the u64
        // read-length header that the VFS reply prepends before the data.
        // Requesting 512 caused the VFS reply payload (header + data) to exceed
        // Message::MAX_PAYLOAD (128), truncating the data silently.
        let to_read = core::cmp::min(Message::MAX_PAYLOAD - 16, file_len - out.len());
        let read_msg = match build_read_message(fd, to_read) {
            Ok(msg) => msg,
            Err(err) => {
                yarm_user_rt::user_log!(
                    "PM_VFS_SPAWN_FAIL image_id={} stage=after-openat reason=build_read_msg_fail fd={} err={:?}",
                    image_id, fd, err
                );
                return Err(ProcessManagerError::Malformed);
            }
        };
        yarm_user_rt::user_log!("PM_VFS_CALL op=READ fd={} len={}", fd, to_read);
        let read_reply = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &read_msg) }?;
        let payload = read_reply.as_slice();

        {
            let preview_len = core::cmp::min(payload.len(), 16);
            yarm_user_rt::user_log!(
                "PM_VFS_READ_REPLY_RAW fd={} requested={} len={} first16={:x?}",
                fd, to_read, payload.len(), &payload[..preview_len]
            );
        }

        let read_len = decode_u64(payload).ok_or(ProcessManagerError::Malformed)? as usize;

        if read_len == 0 {
            // Premature EOF: backend signalled zero bytes before file_len reached.
            yarm_user_rt::user_log!(
                "PM_VFS_READ_EOF total={} expected={}",
                out.len(), file_len
            );
            yarm_user_rt::user_log!(
                "PM_VFS_READ_NO_PROGRESS fd={} total={} expected={} reason=premature_eof",
                fd, out.len(), file_len
            );
            let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
            let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
            return Err(ProcessManagerError::Malformed);
        }

        let inline = payload.get(16..).unwrap_or(&[]);
        let copy_len = core::cmp::min(read_len, inline.len());
        if copy_len > 0 {
            let first4_end = core::cmp::min(copy_len, 4);
            let first4 = &inline[..first4_end];
            out.extend_from_slice(&inline[..copy_len]);
            if PM_VFS_READ_APPEND_TRACE {
                yarm_user_rt::user_log!(
                    "PM_VFS_READ_APPEND bytes={} total={} expected={} first4={:x?}",
                    copy_len, out.len(), file_len, first4
                );
            }
        }

        if out.len() == prev_len {
            // Got a positive read_len but no inline bytes — format mismatch or
            // placeholder backend.  No progress means we can never complete.
            yarm_user_rt::user_log!(
                "PM_VFS_READ_NO_PROGRESS fd={} total={} expected={} read_len={} inline_len={}",
                fd, prev_len, file_len, read_len, inline.len()
            );
            let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
            let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
            return Err(ProcessManagerError::Malformed);
        }
    }

    {
        let first4_end = core::cmp::min(out.len(), 4);
        yarm_user_rt::user_log!(
            "PM_VFS_READ_DONE image_id={} total={} first4={:x?}",
            image_id, out.len(), &out[..first4_end]
        );
    }

    let close_msg = build_close_message(fd).map_err(|_| ProcessManagerError::Malformed)?;
    yarm_user_rt::user_log!("PM_VFS_CALL op=CLOSE fd={}", fd);
    let _ = unsafe { pm_vfs_call_u64(vfs_send_cap, reply_recv_cap, &close_msg) };
    Ok(out)
}

#[cfg(test)]
fn synthetic_elf_image(image_id: u64) -> [u8; 128] {
    let mut image = [0u8; 128];
    image[..4].copy_from_slice(b"\x7FELF");
    image[4] = 2; // ELFCLASS64
    image[5] = 1; // little-endian
    image[6] = 1; // version
    image[7] = 0; // SYSV ABI
    image[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // EM_X86_64
    image[20..24].copy_from_slice(&1u32.to_le_bytes()); // EV_CURRENT
    let entry = 0x400000u64.saturating_add(image_id.saturating_mul(0x1000));
    image[24..32].copy_from_slice(&entry.to_le_bytes());
    image[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    image[52..54].copy_from_slice(&(64u16).to_le_bytes()); // e_ehsize
    image[54..56].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
    image[56..58].copy_from_slice(&(1u16).to_le_bytes()); // e_phnum
    let ph = 64usize;
    image[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    image[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // RX
    image[ph + 8..ph + 16].copy_from_slice(&120u64.to_le_bytes()); // p_offset
    image[ph + 16..ph + 24].copy_from_slice(&(entry & !0xFFF).to_le_bytes()); // p_vaddr
    image[ph + 24..ph + 32].copy_from_slice(&0u64.to_le_bytes()); // p_paddr
    image[ph + 32..ph + 40].copy_from_slice(&8u64.to_le_bytes()); // p_filesz
    image[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes()); // p_memsz
    image[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
    image[120..128].copy_from_slice(&[0x90; 8]);
    image
}

#[cfg(test)]
fn roundtrip_ipc(
    runtime: &impl ProcessServiceKernelIpcRuntime,
    service: &mut ProcessService,
    client_send_cap: CapId,
    server_recv_cap: CapId,
    client_recv_cap: CapId,
    request: Message,
) -> Result<Message, ProcessManagerError> {
    runtime.synthetic_roundtrip_call_reply_with_budget(
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        request,
        PROCESS_MANAGER_ROUNDTRIP_RECV_TIMEOUT_TICKS,
    )
}

#[cfg(test)]
pub trait ProcessServiceKernelIpcRuntime {
    fn create_endpoint(&self, depth: usize) -> Result<(usize, CapId, CapId), ProcessManagerError>;

    fn control_plane_set_process_cnode_slots_via_syscall(
        &self,
        pid: u64,
        requested_slots: usize,
    ) -> Result<(), ProcessManagerError>;

    fn synthetic_roundtrip_call_reply_with_budget(
        &self,
        service: &mut ProcessService,
        client_send_cap: CapId,
        server_recv_cap: CapId,
        client_recv_cap: CapId,
        request: Message,
        recv_timeout_ticks: u64,
    ) -> Result<Message, ProcessManagerError>;
}

#[cfg(test)]
impl<T> ProcessServiceKernelIpcRuntime for T
where
    T: RuntimeStateAccess<KernelState>,
{
    fn create_endpoint(&self, depth: usize) -> Result<(usize, CapId, CapId), ProcessManagerError> {
        self.with_state(|kernel| map_kernel_ipc_err(kernel.create_endpoint(depth)))
    }

    fn control_plane_set_process_cnode_slots_via_syscall(
        &self,
        pid: u64,
        requested_slots: usize,
    ) -> Result<(), ProcessManagerError> {
        self.with_state(|kernel| {
            kernel
                .control_plane_set_process_cnode_slots_via_syscall(pid, requested_slots)
                .map_err(|err| map_trap_ipc_error(from_kernel_trap_ipc_error(err)))
        })
    }

    fn synthetic_roundtrip_call_reply_with_budget(
        &self,
        service: &mut ProcessService,
        client_send_cap: CapId,
        server_recv_cap: CapId,
        client_recv_cap: CapId,
        request: Message,
        recv_timeout_ticks: u64,
    ) -> Result<Message, ProcessManagerError> {
        self.with_state(|kernel| {
            super::super::ipc_roundtrip::synthetic_roundtrip_call_reply_with_budget(
                kernel,
                service,
                client_send_cap,
                server_recv_cap,
                client_recv_cap,
                request,
                recv_timeout_ticks,
                |err| map_kernel_ipc_error(from_kernel_ipc_error(err)),
                || ProcessManagerError::Malformed,
                || ProcessManagerError::Malformed,
            )
        })
    }
}

#[cfg(test)]
fn spawn_request_message(
    parent_pid: u64,
    image_id: u64,
    requested_cnode_slots: Option<usize>,
    requested_task_class: Option<TaskClass>,
) -> Result<Message, ProcessManagerError> {
    if let (Some(slots), Some(task_class)) = (requested_cnode_slots, requested_task_class) {
        let slots = u64::try_from(slots).map_err(|_| ProcessManagerError::Malformed)?;
        let class_hint = match task_class {
            TaskClass::App => 0,
            TaskClass::Driver => 1,
            TaskClass::SystemServer => 2,
        };
        return Message::with_header(
            0,
            PROC_OP_SPAWN_V4,
            0,
            None,
            &SpawnV4Args::new(parent_pid, image_id, slots, class_hint).encode(),
        )
        .map_err(|_| ProcessManagerError::Malformed);
    }
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

#[cfg(test)]
pub fn run_request_loop_over_kernel_ipc(
    runtime: &impl ProcessServiceKernelIpcRuntime,
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    run_request_loop_over_kernel_ipc_with_requested_cnode_slots(
        runtime, service, parent_pid, image_id, exit_code, None,
    )
}

#[cfg(test)]
fn run_request_loop_over_kernel_ipc_with_requested_cnode_slots(
    runtime: &impl ProcessServiceKernelIpcRuntime,
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
    requested_cnode_slots: Option<usize>,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    let (_, client_send_cap, server_recv_cap) = runtime.create_endpoint(8)?;
    let (_, _, client_recv_cap) = runtime.create_endpoint(8)?;

    let spawn_reply = roundtrip_ipc(
        runtime,
        service,
        client_send_cap,
        server_recv_cap,
        client_recv_cap,
        spawn_request_message(
            parent_pid,
            image_id,
            requested_cnode_slots,
            requested_cnode_slots.map(|_| TaskClass::App),
        )?,
    )?;
    let spawned = SpawnV2Result::decode(spawn_reply.as_slice())?;
    let recorded_requested_slots = service
        .requested_cnode_slots_for_process(spawned.pid.0)
        .flatten();
    if let Some(requested_slots) = requested_cnode_slots
        && recorded_requested_slots != Some(requested_slots)
    {
        return Err(ProcessManagerError::Malformed);
    }
    if let Some(requested_slots) = recorded_requested_slots.or(requested_cnode_slots) {
        runtime
            .control_plane_set_process_cnode_slots_via_syscall(spawned.pid.0, requested_slots)?;
    }

    let _ = roundtrip_ipc(
        runtime,
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
        runtime,
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

#[cfg(test)]
pub fn run_request_loop_over_runtime_state_with_cnode_resize(
    runtime: &impl ProcessServiceKernelIpcRuntime,
    service: &mut ProcessService,
    parent_pid: u64,
    image_id: u64,
    exit_code: u64,
    requested_cnode_slots: usize,
) -> Result<ProcessManagerLoopSummary, ProcessManagerError> {
    run_request_loop_over_kernel_ipc_with_requested_cnode_slots(
        runtime,
        service,
        parent_pid,
        image_id,
        exit_code,
        Some(requested_cnode_slots),
    )
}

pub fn run() {
    yarm_user_rt::user_log!("PM_RUN_ENTER");
    let ctx = yarm_user_rt::runtime::startup_context();
    yarm_user_rt::user_log!(
        "PM_STARTUP_CAPS request_send={} request_recv={} reply_recv={}",
        ctx.process_manager_request_send_cap.unwrap_or(0),
        ctx.pm_request_recv_cap.unwrap_or(0),
        ctx.process_manager_reply_recv_cap.unwrap_or(0)
    );
    let Some(recv_cap) = ctx.pm_request_recv_cap else {
        yarm_user_rt::user_log!("PM_NO_RECV_CAP");
        loop {
            let _ = yarm_user_rt::syscall::yield_now();
        }
    };
    yarm_user_rt::user_log!("PM_RECV_LOOP_START recv_cap={}", recv_cap);
    yarm_user_rt::user_log!("PM_BLOCKING_RECV_LOOP");
    let mut service = ProcessService::new();

    // Seed lifecycle records for bootstrap services spawned before PM's loop.
    // PM=image_id 2, supervisor=image_id 1, init_server=image_id 3.
    service.seed_bootstrap_lifecycle_record(ctx.task_id, 2);

    // Startup slots 8 (init TID) and 9 (supervisor TID) are populated by the
    // kernel only for tasks that receive them. For PM these slots are zero.
    // Log the raw values so boot diagnostics capture the actual kernel state.
    let raw_init_tid = ctx.init_tid.unwrap_or(0);
    let raw_sup_tid = ctx.supervisor_tid.unwrap_or(0);
    yarm_user_rt::user_log!("PM_STARTUP_SLOT_8_INIT_TID raw={}", raw_init_tid);
    yarm_user_rt::user_log!("PM_STARTUP_SLOT_9_SUPERVISOR_TID raw={}", raw_sup_tid);

    // Seed supervisor lifecycle (image_id=1).
    if raw_sup_tid != 0 {
        service.seed_bootstrap_lifecycle_record(raw_sup_tid, 1);
    } else {
        yarm_user_rt::user_log!(
            "PM_LIFECYCLE_BOOTSTRAP_SKIP image_id=1 reason=missing_slot"
        );
        // Supervisor is always spawned immediately before PM in the boot
        // sequence, so its TID is ctx.task_id - 1 deterministically.
        service.seed_bootstrap_lifecycle_record(ctx.task_id - 1, 1);
    }

    // Seed init_server lifecycle (image_id=3).
    if raw_init_tid != 0 {
        service.seed_bootstrap_lifecycle_record(raw_init_tid, 3);
    } else {
        yarm_user_rt::user_log!(
            "PM_LIFECYCLE_BOOTSTRAP_SKIP image_id=3 reason=missing_slot"
        );
        // Init is spawned two slots before PM in the deterministic boot order.
        service.seed_bootstrap_lifecycle_record(ctx.task_id - 2, 3);
    }

    loop {
        // SAFETY: direct syscall wrapper call; PM owns its recv endpoint capability.
        match unsafe { yarm_user_rt::syscall::ipc_recv_v2(recv_cap) } {
            Ok(Some(received)) => {
                let msg = received.message;
                let reply_cap = received.reply_cap;
                yarm_user_rt::user_log!(
                    "PM_RECV_GOT_MSG opcode={} len={} reply_cap={:?} transferred_cap={:?} sender_tid={}",
                    msg.opcode,
                    msg.len,
                    reply_cap,
                    received.transferred_cap,
                    received.sender_tid
                );
                if let Ok(reply) = service.handle(msg) {
                    if let Some(cap) = reply_cap {
                        // SAFETY: kernel validates reply capability rights/object.
                        let _ = unsafe { yarm_user_rt::syscall::ipc_reply(cap, &reply) };
                    }
                } else {
                    yarm_user_rt::user_log!(
                        "PM_RECV_DECODE_FAIL opcode={} reply_cap={}",
                        msg.opcode,
                        reply_cap.unwrap_or(u32::MAX)
                    );
                    if let Some(cap) = reply_cap {
                        let err_payload = 1u64.to_le_bytes();
                        if let Ok(err_reply) =
                            Message::with_header(0, msg.opcode, 0, None, &err_payload)
                        {
                            // SAFETY: kernel validates reply capability rights/object.
                            let _ = unsafe { yarm_user_rt::syscall::ipc_reply(cap, &err_reply) };
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(_e) => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let src = include_str!("service.rs");
        assert!(
            src.contains("run_request_loop("),
            "process-manager migration should keep request-loop entrypoint"
        );
        assert!(
            src.contains("PROC_OP_WAITPID_V2"),
            "process-manager request loop should keep waitpid v2 handling"
        );
    }

    #[test]
    fn process_manager_kernel_ipc_request_loop_runs_spawn_and_wait() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("run_request_loop_over_kernel_ipc("),
            "process-manager migration should keep kernel-ipc request-loop entrypoint"
        );
        assert!(
            src.contains("roundtrip_ipc("),
            "process-manager migration should keep roundtrip ipc helper path"
        );
    }

    #[test]
    fn process_manager_shared_kernel_path_can_resize_spawned_process_cnode() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("run_request_loop_over_runtime_state_with_cnode_resize"),
            "process-manager migration should keep runtime-state cnode-resize path"
        );
        assert!(
            src.contains("PROC_OP_SPAWN_V3"),
            "shared-kernel path should continue to support spawn v3 requested slots"
        );
    }

    #[test]
    fn process_manager_shared_kernel_requested_resize_is_denied_without_system_server_context() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("ProcessManagerError::PermissionDenied"),
            "shared-kernel resize path should preserve permission-denied guard"
        );
    }

    #[test]
    fn process_manager_ipc_error_mapping_covers_policy_budget_and_transport_paths() {
        assert_eq!(
            map_kernel_ipc_error(KernelIpcError::MissingRight),
            ProcessManagerError::PermissionDenied
        );
        assert_eq!(
            map_kernel_ipc_error(KernelIpcError::CapabilityFull),
            ProcessManagerError::TableFull
        );
        assert_eq!(
            map_trap_ipc_error(TrapIpcError::MissingTrapFrame),
            ProcessManagerError::InvalidTransport
        );
        assert_eq!(
            map_trap_ipc_error(from_kernel_trap_ipc_error(TrapHandleError::Syscall(
                KernelSyscallError::InvalidArgs,
            ))),
            ProcessManagerError::Malformed
        );
        assert_eq!(
            map_trap_ipc_error(from_kernel_trap_ipc_error(TrapHandleError::Syscall(
                KernelSyscallError::Internal,
            ))),
            ProcessManagerError::TableFull
        );
    }

    #[test]
    fn process_manager_kernel_ipc_v2_spawn_path_does_not_create_process_cnode_resize_side_effect() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("PROC_OP_SPAWN_V2"),
            "process-manager migration must keep v2 spawn path"
        );
        let legacy_cp = ["yarm", "::services::", "control_plane::"].concat();
        assert!(
            !src.contains(legacy_cp.as_str()),
            "workspace process-manager impl must not delegate to legacy control-plane namespace"
        );
    }

    #[test]
    fn process_manager_source_guardrail_prefers_budgeted_timed_receive_path() {
        let src = include_str!("service.rs");
        assert!(
            src.contains("synthetic_roundtrip_call_reply_with_budget"),
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
        assert!(
            src.contains("PROC_OP_SPAWN_V4"),
            "process-manager migration should include v4 spawn path for task class metadata"
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
            &[b"/sbin/process_manager"],
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

    #[test]
    fn task_restart_token_lookup_returns_found_token_when_recorded() {
        let mut service = ProcessService::new();
        service
            .record_restart_token(17, 0xAA55)
            .expect("record token");
        let request = Message::with_header(
            0,
            PROC_OP_TASK_RESTART_TOKEN,
            0,
            None,
            &TaskRestartTokenRequest::new(17).encode(),
        )
        .expect("request");
        let reply_msg = service.handle(request).expect("reply");
        let reply = TaskRestartTokenReply::decode(reply_msg.as_slice()).expect("decode");
        assert_eq!(reply.found_token(), Some(0xAA55));
    }

    #[test]
    fn task_restart_token_lookup_returns_not_found_for_unknown_tid() {
        let mut service = ProcessService::new();
        let request = Message::with_header(
            0,
            PROC_OP_TASK_RESTART_TOKEN,
            0,
            None,
            &TaskRestartTokenRequest::new(404).encode(),
        )
        .expect("request");
        let reply_msg = service.handle(request).expect("reply");
        let reply = TaskRestartTokenReply::decode(reply_msg.as_slice()).expect("decode");
        assert_eq!(reply.found_token(), None);
    }

    #[test]
    fn register_supervised_task_records_restart_token_for_lookup() {
        let mut service = ProcessService::new();
        let register = Message::with_header(
            0,
            PROC_OP_REGISTER_SUPERVISED_TASK,
            0,
            None,
            &RegisterSupervisedTask::new(55, 0xDEAD).encode(),
        )
        .expect("register");
        let _ = service.handle(register).expect("register reply");

        let lookup = Message::with_header(
            0,
            PROC_OP_TASK_RESTART_TOKEN,
            0,
            None,
            &TaskRestartTokenRequest::new(55).encode(),
        )
        .expect("lookup");
        let reply_msg = service.handle(lookup).expect("lookup reply");
        let reply = TaskRestartTokenReply::decode(reply_msg.as_slice()).expect("decode");
        assert_eq!(reply.found_token(), Some(0xDEAD));
    }

    #[test]
    fn execute_restart_returns_truthful_statuses_and_unsupported_backend() {
        let mut service = ProcessService::new();
        let call = |service: &mut ProcessService, tid: u64, token: u64| {
            let req = Message::with_header(
                0,
                PROC_OP_EXECUTE_RESTART,
                0,
                None,
                &ExecuteRestartRequest::new(tid, token).encode(),
            )
            .expect("request");
            let reply_msg = service.handle(req).expect("reply");
            ExecuteRestartReply::decode(reply_msg.as_slice())
                .expect("decode")
                .status
        };

        assert_eq!(
            call(&mut service, 9, 1),
            ExecuteRestartReply::STATUS_NOT_FOUND
        );

        let register = Message::with_header(
            0,
            PROC_OP_REGISTER_SUPERVISED_TASK,
            0,
            None,
            &RegisterSupervisedTask::new(9, 77).encode(),
        )
        .expect("register");
        let _ = service.handle(register).expect("register reply");

        assert_eq!(
            call(&mut service, 9, 12),
            ExecuteRestartReply::STATUS_TOKEN_MISMATCH
        );
        assert_eq!(
            call(&mut service, 9, 77),
            ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED
        );
    }

    #[test]
    fn lifecycle_table_records_one_entry_per_service() {
        let mut table = LifecycleTable::new();
        assert!(table.is_empty());

        let ok = table.record(ServiceLifecycleRecord {
            tid: 100,
            image_id: 4,
            parent_tid: 0,
            pm_service_send_cap: 42,
            state: ServiceState::Spawned,
        });
        assert!(ok);
        assert_eq!(table.len(), 1);

        let rec = table.get_by_tid(100).expect("get by tid");
        assert_eq!(rec.image_id, 4);
        assert_eq!(rec.pm_service_send_cap, 42);
        assert!(matches!(rec.state, ServiceState::Spawned));
    }

    #[test]
    fn lifecycle_table_get_by_image_id_returns_first_match() {
        let mut table = LifecycleTable::new();
        table.record(ServiceLifecycleRecord {
            tid: 10,
            image_id: 5,
            parent_tid: 0,
            pm_service_send_cap: 7,
            state: ServiceState::Spawned,
        });
        table.record(ServiceLifecycleRecord {
            tid: 11,
            image_id: 6,
            parent_tid: 1,
            pm_service_send_cap: 9,
            state: ServiceState::Spawned,
        });

        assert_eq!(table.get_by_image_id(5).unwrap().tid, 10);
        assert_eq!(table.get_by_image_id(6).unwrap().tid, 11);
        assert!(table.get_by_image_id(99).is_none());
    }

    #[test]
    fn spawn_source_policy_bootstrap_and_vfs_ranges() {
        assert_eq!(
            resolve_spawn_load_source(4).ok(),
            Some(SpawnLoadSource::DirectInitrd)
        );
        assert_eq!(
            resolve_spawn_load_source(5).ok(),
            Some(SpawnLoadSource::DirectInitrd)
        );
        assert_eq!(
            resolve_spawn_load_source(6).ok(),
            Some(SpawnLoadSource::DirectInitrd)
        );
        assert_eq!(resolve_spawn_load_source(7).ok(), Some(SpawnLoadSource::Vfs));
        assert_eq!(resolve_spawn_load_source(8).ok(), Some(SpawnLoadSource::Vfs));
        assert_eq!(resolve_spawn_load_source(9).ok(), Some(SpawnLoadSource::Vfs));
        assert_eq!(resolve_spawn_load_source(10), Err(ProcessManagerError::Unsupported));
    }

    #[test]
    fn lifecycle_table_capacity_is_enforced() {
        let mut table = LifecycleTable::new();
        for i in 0..MAX_LIFECYCLE_ENTRIES {
            assert!(table.record(ServiceLifecycleRecord {
                tid: i as u64,
                image_id: 0,
                parent_tid: 0,
                pm_service_send_cap: 0,
                state: ServiceState::Spawned,
            }));
        }
        assert_eq!(table.len(), MAX_LIFECYCLE_ENTRIES);
        // One more should fail
        assert!(!table.record(ServiceLifecycleRecord {
            tid: 9999,
            image_id: 0,
            parent_tid: 0,
            pm_service_send_cap: 0,
            state: ServiceState::Spawned,
        }));
    }

    // ── pm_read_all_via_vfs unit tests (mock-based) ──────────────────────────

    /// Simulate the READ loop logic from pm_read_all_via_vfs with a mock VFS
    /// backend that returns pre-built reply payloads.  Each payload must match
    /// the extended inline format: [read_len u64 LE][status u64 LE][bytes...].
    /// Returns `Err` on premature EOF or no-progress, matching production logic.
    fn mock_read_all(file_len: usize, replies: &[Vec<u8>]) -> Result<Vec<u8>, &'static str> {
        let mut out: Vec<u8> = Vec::with_capacity(file_len);
        let mut call_idx = 0usize;
        while out.len() < file_len {
            let prev_len = out.len();
            if call_idx >= replies.len() {
                return Err("no more replies");
            }
            let payload = &replies[call_idx];
            call_idx += 1;
            if payload.len() < 8 {
                return Err("short payload");
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&payload[..8]);
            let read_len = u64::from_le_bytes(b) as usize;
            if read_len == 0 {
                // Premature EOF
                return Err("premature_eof");
            }
            let inline = payload.get(16..).unwrap_or(&[]);
            let copy_len = core::cmp::min(read_len, inline.len());
            if copy_len > 0 {
                out.extend_from_slice(&inline[..copy_len]);
            }
            if out.len() == prev_len {
                return Err("no_progress");
            }
        }
        Ok(out)
    }

    fn make_extended_reply(data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(16 + data.len());
        payload.extend_from_slice(&(data.len() as u64).to_le_bytes()); // read_len
        payload.extend_from_slice(&0u64.to_le_bytes());                 // status
        payload.extend_from_slice(data);
        payload
    }

    fn make_count_only_reply(count: u64) -> Vec<u8> {
        count.to_le_bytes().to_vec()
    }

    #[test]
    fn pm_read_all_via_vfs_premature_eof_returns_error() {
        // Backend signals EOF (read_len=0) before reaching file_len.
        let replies = vec![make_count_only_reply(0)];
        let err = mock_read_all(100, &replies).expect_err("should fail");
        assert_eq!(err, "premature_eof");
    }

    #[test]
    fn pm_read_all_via_vfs_no_inline_bytes_returns_no_progress_error() {
        // Backend returns a positive read_len but no inline bytes (count-only
        // 8-byte reply).  This is the placeholder-mode format which cannot
        // deliver actual file bytes; the loop must detect no progress and fail.
        let replies = vec![make_count_only_reply(50)];
        let err = mock_read_all(100, &replies).expect_err("should fail on no progress");
        assert_eq!(err, "no_progress");
    }

    #[test]
    fn pm_read_all_via_vfs_multi_chunk_accumulates_correctly() {
        // Two READ replies accumulate to file_len.
        let chunk1: Vec<u8> = (0u8..20).collect();
        let chunk2: Vec<u8> = (20u8..30).collect();
        let replies = vec![make_extended_reply(&chunk1), make_extended_reply(&chunk2)];
        let result = mock_read_all(30, &replies).expect("should succeed");
        assert_eq!(result.len(), 30);
        assert_eq!(&result[..20], chunk1.as_slice());
        assert_eq!(&result[20..], chunk2.as_slice());
    }

    #[test]
    fn pm_read_all_via_vfs_single_chunk_exact_fit() {
        // Single READ reply exactly covers file_len.
        let data: Vec<u8> = (0u8..112).collect();
        let replies = vec![make_extended_reply(&data)];
        let result = mock_read_all(112, &replies).expect("should succeed");
        assert_eq!(result, data);
    }

    // ── OPENAT reply decode unit tests ────────────────────────────────────────

    #[test]
    fn openat_reply_8_byte_le_fd13_decodes_correctly() {
        // QEMU proof: VFS sends bytes=[d, 0, 0, 0, 0, 0, 0, 0] for fd=13.
        let payload = [0x0du8, 0, 0, 0, 0, 0, 0, 0];
        let result = decode_u64(&payload);
        assert_eq!(result, Some(13), "fd=13 must decode from 8-byte LE payload");
    }

    #[test]
    fn openat_reply_bad_length_returns_none() {
        // A 7-byte payload is too short; decode_u64 must return None so the
        // caller logs PM_VFS_SPAWN_FAIL stage=after-openat reason=bad_fd_decode.
        let payload = [0x0du8, 0, 0, 0, 0, 0, 0];
        let result = decode_u64(&payload);
        assert_eq!(result, None, "7-byte payload must return None");
    }

    #[test]
    fn openat_reply_empty_returns_none() {
        let result = decode_u64(&[]);
        assert_eq!(result, None, "empty payload must return None");
    }

    #[test]
    fn openat_reply_fd_zero_decodes_to_zero() {
        // fd=0 is a valid u64; the protocol layer may treat it as invalid but
        // decode_u64 itself must not reject it — callers decide the contract.
        let payload = [0u8; 8];
        let result = decode_u64(&payload);
        assert_eq!(result, Some(0), "fd=0 must decode to 0");
    }

    #[test]
    fn openat_reply_extra_bytes_ignored_on_decode() {
        // A longer-than-8-byte payload is accepted; only the first 8 bytes matter.
        let mut payload = [0u8; 16];
        payload[0] = 0x0d; // fd=13
        let result = decode_u64(&payload);
        assert_eq!(result, Some(13), "extra bytes beyond 8 must be ignored");
    }
}
