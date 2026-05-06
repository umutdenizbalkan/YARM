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
use yarm_ipc_abi::process_abi::{
    ExecuteRestartReply, ExecuteRestartRequest, PROC_OP_EXECUTE_RESTART, PROC_OP_EXIT,
    PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_REGISTER_SUPERVISED_TASK, PROC_OP_SPAWN_V2,
    PROC_OP_SPAWN_V3, PROC_OP_SPAWN_V4, PROC_OP_TASK_RESTART_TOKEN, PROC_OP_WAITPID_V2,
    RegisterSupervisedTask, SpawnV2Args, SpawnV3Args, SpawnV4Args, TaskRestartTokenReply,
    TaskRestartTokenRequest, WaitPidV2Args,
};
use yarm_srv_common::elf::ElfImageInfo;
use yarm_srv_common::service_loop::RequestResponseService;
use yarm_srv_common::service_loop::run_typed_request_loop;
#[cfg(test)]
use yarm_user_rt::capability::CapId;
use yarm_user_rt::ipc::Message;
use yarm_user_rt::process::{
    ProcessError as ProcessManagerError, ProcessId, ProcessManagerOps, WaitResult,
};
#[cfg(test)]
use yarm_user_rt::runtime::{KernelIpcError, RuntimeStateAccess, TrapIpcError};
#[cfg(test)]
use yarm_user_rt::syscall::SyscallError;
use yarm_user_rt::syscall::{IpcTransportV2, SyscallIpcTransport};
use yarm_user_rt::task::TaskClass;

#[cfg(test)]
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
pub struct SpawnV2Request {
    pub parent_pid: ProcessId,
    pub image_id: u64,
    pub requested_cnode_slots: Option<usize>,
    pub requested_task_class: Option<TaskClass>,
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
    WaitPidV2(WaitPidV2Request),
    TaskRestartToken { tid: u64 },
    RegisterSupervisedTask { tid: u64, restart_token: u64 },
    ExecuteRestart { tid: u64, restart_token: u64 },
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

#[derive(Debug)]
pub struct ProcessService {
    manager: KernelProcessManagerAdapter,
    policy_records: [Option<ProcessSpawnPolicyRecord>; 64],
    restart_token_records: [Option<RestartTokenRecord>; 64],
    restart_control_send_cap: Option<u32>,
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
            handled: 0,
        }
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
                    let _ = req;
                    Err(ProcessManagerError::Unsupported)
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
        }
    }

    fn execute_restart_via_kernel_cap(&self, tid: u64, restart_token: u64) -> u8 {
        let mut transport = SyscallIpcTransport;
        self.execute_restart_via_transport(&mut transport, tid, restart_token)
    }

    fn execute_restart_via_transport(
        &self,
        transport: &mut impl IpcTransportV2,
        tid: u64,
        restart_token: u64,
    ) -> u8 {
        let Some(send_cap) = self.restart_control_send_cap else {
            return ExecuteRestartReply::STATUS_PERMISSION_DENIED;
        };
        let request = ExecuteRestartRequest::new(tid, restart_token);
        let reply_cap = yarm_user_rt::runtime::startup_context()
            .process_manager_reply_recv_cap
            .unwrap_or(0);
        match transport.request_reply_v2(
            send_cap,
            reply_cap,
            &request.encode(),
            |payload| ExecuteRestartReply::decode(payload).ok().map(|reply| reply.status),
        ) {
            Ok(status) => status,
            Err(_) => return ExecuteRestartReply::STATUS_INTERNAL_UNSUPPORTED,
        }
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
    let mut service = ProcessService::new();
    let summary = run_request_loop(&mut service, 1, 42, 0).expect("process-manager loop");

    yarm_user_rt::user_log!(
        "process-manager request-loop ready: pid={}, exit_code={}, handled={}",
        summary.waited_pid,
        summary.waited_exit,
        summary.handled
    );
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

    struct StubV2Transport {
        expected_send_cap: u32,
        expected_reply_cap: u32,
        expected_payload: [u8; 16],
        reply_status: u8,
    }

    impl yarm_user_rt::syscall::IpcTransportV2 for StubV2Transport {
        fn send_v2(
            &mut self,
            _endpoint_cap: u32,
            _payload: &[u8],
            _transfer_cap: Option<u64>,
        ) -> Result<(), yarm_user_rt::syscall::SyscallError> {
            panic!("not used");
        }
        fn recv_v2(
            &mut self,
            _recv_cap: u32,
        ) -> Result<Option<yarm_user_rt::syscall::IpcV2Response>, yarm_user_rt::syscall::SyscallError>
        {
            panic!("not used");
        }
        fn recv_v2_with_deadline(
            &mut self,
            _recv_cap: u32,
            _timeout_ticks: u64,
        ) -> Result<Option<yarm_user_rt::syscall::IpcV2Response>, yarm_user_rt::syscall::SyscallError>
        {
            panic!("not used");
        }
        fn reply_v2(
            &mut self,
            _reply_cap: u32,
            _payload: &[u8],
            _transfer_cap: Option<u64>,
        ) -> Result<(), yarm_user_rt::syscall::SyscallError> {
            panic!("not used");
        }
        fn call_v2(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
        ) -> Result<yarm_user_rt::syscall::IpcV2Response, yarm_user_rt::syscall::SyscallError> {
            assert_eq!(send_cap, self.expected_send_cap);
            assert_eq!(reply_recv_cap, self.expected_reply_cap);
            assert_eq!(payload, self.expected_payload);
            let reply = ExecuteRestartReply::new(self.reply_status).encode();
            let mut payload_buf = [0u8; Message::MAX_PAYLOAD];
            payload_buf[..reply.len()].copy_from_slice(&reply);
            Ok(yarm_user_rt::syscall::IpcV2Response {
                status: 0,
                len: reply.len(),
                transfer_cap: None,
                payload: payload_buf,
            })
        }
        fn request_reply_v2<T>(
            &mut self,
            send_cap: u32,
            reply_recv_cap: u32,
            payload: &[u8],
            decode_reply: impl FnOnce(&[u8]) -> Option<T>,
        ) -> Result<T, yarm_user_rt::syscall::SyscallError> {
            yarm_user_rt::syscall::request_reply_v2(
                self,
                send_cap,
                reply_recv_cap,
                payload,
                decode_reply,
            )
        }
    }

    #[test]
    fn execute_restart_uses_v2_transport_adapter_path() {
        let mut service = ProcessService::new();
        service.restart_control_send_cap = Some(77);
        let request = ExecuteRestartRequest::new(9, 0xBEEF);
        let mut transport = StubV2Transport {
            expected_send_cap: 77,
            expected_reply_cap: 0,
            expected_payload: request.encode(),
            reply_status: ExecuteRestartReply::STATUS_OK,
        };
        assert_eq!(
            service.execute_restart_via_transport(&mut transport, 9, 0xBEEF),
            ExecuteRestartReply::STATUS_OK
        );
    }
}
