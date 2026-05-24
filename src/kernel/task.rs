// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::capabilities::CapId;
use super::ipc::ThreadId;
use super::scheduler::CpuId;
use super::vm::{Asid, VirtAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RestartToken(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadGroupId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    EndpointReceive(CapId),
    EndpointSend(CapId),
    Futex(VirtAddr),
    Join(ThreadId),
    Poll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvAbiVariant {
    RecvV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedRecvState {
    pub recv_cap: CapId,
    pub payload_user_ptr: usize,
    pub payload_user_len: usize,
    pub meta_user_ptr: usize,
    pub meta_user_len: usize,
    pub recv_abi: RecvAbiVariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClass {
    App,
    Driver,
    SystemServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPolicy {
    KillTask,
    NotifyAndContinue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Runnable,
    /// Set only by `KernelState::dispatch_next_task()` / yield scheduling paths.
    /// Do not assign directly outside scheduler-mediated transitions.
    Running,
    Blocked(WaitReason),
    Faulted,
    Exited(u64),
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadDetachState {
    Joinable,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserRegisterContext {
    pub instruction_ptr: VirtAddr,
    pub stack_ptr: VirtAddr,
    pub user_gprs: [usize; 32],
    pub arg0: usize,
    pub arg1: usize,
    pub arg2: usize,
    pub arg3: usize,
    pub arg4: usize,
    pub arg5: usize,
}

impl Default for UserRegisterContext {
    fn default() -> Self {
        Self {
            instruction_ptr: VirtAddr(0),
            stack_ptr: VirtAddr(0),
            user_gprs: [0; 32],
            arg0: 0,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RobustFutexState {
    pub head: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestartState {
    pub token: Option<RestartToken>,
}

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchSwitchContext {
    words: [usize; 8],
    fxsave: [u8; 512],
}

impl Default for ArchSwitchContext {
    fn default() -> Self {
        Self {
            words: [0; 8],
            fxsave: [0; 512],
        }
    }
}

impl ArchSwitchContext {
    pub const WORDS: usize = 8;
    pub const FXSAVE_BYTES: usize = 512;
    const STACK_PTR_IDX: usize = 0;
    const INSTRUCTION_PTR_IDX: usize = 1;

    pub const fn stack_ptr(self) -> usize {
        self.words[Self::STACK_PTR_IDX]
    }

    pub fn set_stack_ptr(&mut self, value: usize) {
        self.words[Self::STACK_PTR_IDX] = value;
    }

    pub const fn instruction_ptr(self) -> usize {
        self.words[Self::INSTRUCTION_PTR_IDX]
    }

    pub fn set_instruction_ptr(&mut self, value: usize) {
        self.words[Self::INSTRUCTION_PTR_IDX] = value;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KernelExecutionContext {
    pub stack_base: Option<VirtAddr>,
    pub stack_top: Option<VirtAddr>,
    pub frame: ArchSwitchContext,
    pub initialized: bool,
    pub owns_stack: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadControlBlock {
    pub tid: ThreadId,
    pub thread_group_id: ThreadGroupId,
    pub status: TaskStatus,
    pub asid: Option<Asid>,
    pub tls_ptr: Option<VirtAddr>,
    pub user_entry: Option<VirtAddr>,
    pub user_stack_top: Option<VirtAddr>,
    pub user_context: UserRegisterContext,
    pub detach_state: ThreadDetachState,
    /// `None` means fallback to kernel/class policy in `KernelState`.
    pub fault_policy_override: Option<FaultPolicy>,
    pub restart: RestartState,
    pub kernel_context: KernelExecutionContext,
    /// If set, scheduler enqueues this task only on the selected CPU.
    pub cpu_affinity: Option<CpuId>,
    /// Absolute scheduler tick at which an IPC wait should timeout.
    pub ipc_timeout_deadline: Option<u64>,
    /// Set when a blocked IPC wait is resumed due to timeout expiry.
    pub ipc_timeout_fired: bool,
    /// Saved userspace recv buffers for blocked recv-v2 completion.
    pub blocked_recv_state: Option<BlockedRecvState>,
}

impl ThreadControlBlock {
    pub fn new(tid: ThreadId, asid: Option<Asid>) -> Self {
        Self {
            tid,
            thread_group_id: ThreadGroupId(tid.0),
            status: TaskStatus::Runnable,
            asid,
            tls_ptr: None,
            user_entry: None,
            user_stack_top: None,
            user_context: UserRegisterContext::default(),
            detach_state: ThreadDetachState::Joinable,
            fault_policy_override: None,
            restart: RestartState::default(),
            kernel_context: KernelExecutionContext::default(),
            cpu_affinity: None,
            ipc_timeout_deadline: None,
            ipc_timeout_fired: false,
            blocked_recv_state: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_variants_construct() {
        let _ = TaskStatus::Runnable;
        let _ = TaskStatus::Running;
        let _ = TaskStatus::Blocked(WaitReason::Poll);
        let _ = TaskStatus::Blocked(WaitReason::Futex(VirtAddr(0x1000)));
        let _ = TaskStatus::Blocked(WaitReason::Join(ThreadId(7)));
        let _ = TaskStatus::Faulted;
        let _ = TaskStatus::Exited(0);
        let _ = TaskStatus::Dead;
    }

    #[test]
    fn tcb_constructor_uses_typed_fields() {
        let mut tcb = ThreadControlBlock::new(ThreadId(7), Some(Asid(1)));
        tcb.tls_ptr = Some(VirtAddr(0xDEAD_BEEF));
        tcb.user_entry = Some(VirtAddr(0x4000));
        tcb.user_stack_top = Some(VirtAddr(0x8000));
        tcb.user_context = UserRegisterContext {
            instruction_ptr: VirtAddr(0x4000),
            stack_ptr: VirtAddr(0x8000),
            arg0: 1,
            arg1: 2,
            arg2: 3,
            arg3: 4,
            arg4: 5,
            arg5: 6,
        };
        tcb.fault_policy_override = Some(FaultPolicy::KillTask);
        tcb.restart = RestartState {
            token: Some(RestartToken(9)),
        };
        tcb.kernel_context.stack_base = Some(VirtAddr(0x9000));
        tcb.kernel_context.stack_top = Some(VirtAddr(0xA000));
        tcb.kernel_context.frame.set_stack_ptr(0x9FF0);
        tcb.kernel_context.frame.set_instruction_ptr(0x1234);
        tcb.kernel_context.initialized = true;
        tcb.kernel_context.owns_stack = true;

        assert_eq!(tcb.tid, ThreadId(7));
        assert_eq!(tcb.restart.token, Some(RestartToken(9)));
        assert_eq!(tcb.thread_group_id, ThreadGroupId(7));
        assert_eq!(tcb.tls_ptr, Some(VirtAddr(0xDEAD_BEEF)));
        assert_eq!(tcb.user_context.instruction_ptr, VirtAddr(0x4000));
        assert_eq!(tcb.detach_state, ThreadDetachState::Joinable);
        assert_eq!(tcb.status, TaskStatus::Runnable);
        assert_eq!(tcb.kernel_context.stack_top, Some(VirtAddr(0xA000)));
        assert_eq!(tcb.kernel_context.frame.stack_ptr(), 0x9FF0);
        assert_eq!(tcb.kernel_context.frame.instruction_ptr(), 0x1234);
        assert!(tcb.kernel_context.initialized);
        assert!(tcb.kernel_context.owns_stack);
    }

    #[test]
    fn tcb_constructor_preserves_large_tid_for_thread_group() {
        let tid = ThreadId(70_000);
        let tcb = ThreadControlBlock::new(tid, None);

        assert_eq!(tcb.thread_group_id, ThreadGroupId(70_000));
    }
}
