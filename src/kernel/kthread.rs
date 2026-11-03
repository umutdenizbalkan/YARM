// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! KTHREAD1 — independent kernel threads with bounded storage and explicit scheduling points.
//!
//! A kernel thread is a privileged continuation with its own stack, scheduled by the ordinary
//! scheduler. Its incarnation id is queued on its home CPU's run queue under that CPU's 1L, the
//! one queue-advance selection owner picks it, and the drain owner that can run it
//! (`queue_advance_acquire_incoming_split`) runs it as a coroutine on the dispatcher's stack. It
//! leaves the CPU only at an explicit scheduling point: [`KThreadCtx::yield_now`], a wait
//! ([`KWaitQueue::wait_until`]), [`KThreadCtx::exit`], or [`KThreadCtx::sched_point`], which yields
//! when an interrupt left a reschedule pending. Interrupts are taken while it runs, serviced on its
//! own stack and returned to the interrupted instruction; they never switch it. Contract:
//! `doc/SCHEDULING_CONTRACT.md` §18.
//!
//! Storage is fixed: [`KTHREAD_CAPACITY`] slots, each with one stack reserved at link time.
//! Creation, scheduling and interrupts allocate nothing. A slot carries a generation; the thread
//! id names `(slot, generation)`, so a stale id never reaches a later incarnation.

use core::cell::{Cell, UnsafeCell};
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::arch::kthread as arch;
use crate::kernel::ipc::ThreadId;
use crate::kernel::lock::SpinLockIrq;
use crate::kernel::scheduler::{CpuId, MAX_CPUS, SchedulerError, TaskPriority};
use crate::runtime::SharedKernel;

/// The number of kernel threads that can exist at once.
pub const KTHREAD_CAPACITY: usize = 32;
/// Bytes of one kernel-thread stack (production). The hosted model uses larger stacks because
/// unoptimised host code is much deeper; the production figure is what the live witness measures.
#[cfg(not(feature = "hosted-dev"))]
pub const KTHREAD_STACK_BYTES: usize = 16 * 1024;
#[cfg(feature = "hosted-dev")]
pub const KTHREAD_STACK_BYTES: usize = 256 * 1024;
/// Each stack starts on a page boundary; the initial stack pointer is 16-byte aligned.
pub const KTHREAD_STACK_ALIGN: usize = 4096;
/// The byte every stack is filled with at creation, so the deepest use can be measured.
pub const KTHREAD_STACK_PAINT: u8 = 0x6B;

const TID_TAG: u64 = 0x4B54 << 48;
const TID_TAG_MASK: u64 = 0xFFFF << 48;
const SLOT_MASK: u64 = 0xFFFF;

/// `true` for an id in the kernel-thread range. User thread ids are small counters and never
/// carry this tag.
pub const fn is_kthread_tid(raw: u64) -> bool {
    raw & TID_TAG_MASK == TID_TAG
}

/// One kernel-thread incarnation: its slot and that slot's generation at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KThreadId(u64);

impl KThreadId {
    const fn encode(slot: usize, generation: u32) -> Self {
        Self(TID_TAG | ((generation as u64) << 16) | slot as u64)
    }

    /// Decode a raw id. `None` for an id outside the kernel-thread range or naming no slot.
    pub fn from_raw(raw: u64) -> Option<Self> {
        if !is_kthread_tid(raw) || (raw & SLOT_MASK) as usize >= KTHREAD_CAPACITY {
            return None;
        }
        Some(Self(raw))
    }

    pub fn raw(self) -> u64 {
        self.0
    }

    pub fn slot(self) -> usize {
        (self.0 & SLOT_MASK) as usize
    }

    pub fn generation(self) -> u32 {
        (self.0 >> 16) as u32
    }

    pub fn tid(self) -> ThreadId {
        ThreadId(self.0)
    }
}

/// A kernel thread's body. It receives its context and the argument given at creation; returning
/// exits the thread.
pub type KEntry = fn(&KThreadCtx, usize);

/// The lifecycle of one slot. Every transition of a live incarnation (Ready, Running, Blocked,
/// Exited) is made under the home CPU's 1L, together with the run-queue change it implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KState {
    /// In the free list; no id names it.
    Free = 0,
    /// Being created; not yet queued.
    Reserved = 1,
    /// Queued on its home CPU.
    Ready = 2,
    /// Current on its home CPU (its stack may be live).
    Running = 3,
    /// Parked by a wait; in no queue.
    Blocked = 4,
    /// Exited; in no queue. Its stack is reclaimed only after its CPU has switched away.
    Exited = 5,
}

impl KState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Reserved,
            2 => Self::Ready,
            3 => Self::Running,
            4 => Self::Blocked,
            5 => Self::Exited,
            _ => Self::Free,
        }
    }
}

const FREE: u8 = KState::Free as u8;
const RESERVED: u8 = KState::Reserved as u8;
const READY: u8 = KState::Ready as u8;
const RUNNING: u8 = KState::Running as u8;
const BLOCKED: u8 = KState::Blocked as u8;
const EXITED: u8 = KState::Exited as u8;

/// Why a kernel thread last left its CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KSwitchReason {
    None = 0,
    Yield = 1,
    Block = 2,
    Exit = 3,
}

impl KSwitchReason {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Yield,
            2 => Self::Block,
            3 => Self::Exit,
            _ => Self::None,
        }
    }
}

/// Why creation failed. Nothing is left allocated or queued by a failed creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KCreateError {
    /// Every slot is in use.
    Exhausted,
    /// The home CPU is out of range.
    InvalidCpu,
    /// The home CPU's scheduler refused the placement (offline, wake-only, queue full).
    Placement(SchedulerError),
    /// Stack storage could not be provided (hosted model only).
    NoStorage,
}

/// Why a switch was refused. A refused switch changes nothing; the caller keeps running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KSwitchRefusal {
    /// Interrupts are masked: an interrupt-masking guard (any `SpinLockIrqGuard`) or a masked
    /// section is live, or the caller is in interrupt context.
    IrqMasked,
    /// A [`KMasked`] section of this thread is open.
    GuardHeld,
    /// This CPU's stack-residency record does not name the caller.
    NotResident,
    /// The stack pointer is outside the caller's own stack.
    WrongStack,
    /// The caller's slot no longer holds this incarnation in the Running state.
    Stale,
    /// The scheduler refused the transition (its queue is full, or the caller is not current).
    Scheduler,
}

impl KSwitchRefusal {
    pub fn marker(self) -> &'static str {
        match self {
            Self::IrqMasked => "irq_masked",
            Self::GuardHeld => "guard_held",
            Self::NotResident => "not_resident",
            Self::WrongStack => "wrong_stack",
            Self::Stale => "stale",
            Self::Scheduler => "scheduler",
        }
    }
}

/// The result of a wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KWake {
    /// The thread was Blocked: it is now Ready and queued on its home CPU.
    Enqueued,
    /// The thread had not parked yet (Running or Ready): its next park returns at once.
    Pending,
    /// The id names an incarnation that no longer exists (exited, reused, never created).
    Stale,
}

/// Why a kernel-origin interrupt was not accepted as a kernel-thread interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KIrqRefusal {
    /// The interrupted stack pointer is in no kernel-thread stack.
    NotKernelThreadStack,
    /// The CPU the hardware names is not the thread's home.
    CpuMismatch,
    /// The home CPU's residency record names no kernel thread.
    NotRunning,
    /// The residency record names a different slot than the interrupted stack's.
    StackMismatch,
    /// The slot does not hold the resident incarnation in the Running state.
    Stale,
}

impl KIrqRefusal {
    pub fn marker(self) -> &'static str {
        match self {
            Self::NotKernelThreadStack => "not_kthread_stack",
            Self::CpuMismatch => "cpu_mismatch",
            Self::NotRunning => "not_running",
            Self::StackMismatch => "stack_mismatch",
            Self::Stale => "stale",
        }
    }
}

/// One slot's control block. Its switch context is touched only by its home CPU with interrupts
/// masked: by the dispatcher entering it and by the thread leaving.
pub(crate) struct KSlot {
    state: AtomicU8,
    generation: AtomicU32,
    home: AtomicU8,
    priority: AtomicU8,
    wake_pending: AtomicBool,
    reason: AtomicU8,
    entry: AtomicUsize,
    arg: AtomicUsize,
    ctx: UnsafeCell<arch::KSwitchContext>,
    dispatches: AtomicU64,
    interrupts: AtomicU64,
}

impl KSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(FREE),
            generation: AtomicU32::new(0),
            home: AtomicU8::new(0),
            priority: AtomicU8::new(TaskPriority::Normal as u8),
            wake_pending: AtomicBool::new(false),
            reason: AtomicU8::new(KSwitchReason::None as u8),
            entry: AtomicUsize::new(0),
            arg: AtomicUsize::new(0),
            ctx: UnsafeCell::new(arch::KSwitchContext::zeroed()),
            dispatches: AtomicU64::new(0),
            interrupts: AtomicU64::new(0),
        }
    }

    fn priority(&self) -> TaskPriority {
        match self.priority.load(Ordering::Acquire) {
            0 => TaskPriority::High,
            2 => TaskPriority::Low,
            _ => TaskPriority::Normal,
        }
    }
}

/// One CPU's kernel-thread state.
pub(crate) struct KCpu {
    /// The stack-residency record: the raw id of the kernel thread whose stack is live on this CPU
    /// (0 = none). Set by the dispatcher immediately before entering it and cleared only after the
    /// thread has switched away, so a stack is never reclaimed or re-entered while live.
    running: AtomicU64,
    /// A reschedule an interrupt left for the running kernel thread's next scheduling point.
    need_resched: AtomicBool,
    /// The dispatcher's continuation while a kernel thread runs.
    dispatcher: UnsafeCell<arch::KSwitchContext>,
    dispatches: AtomicU64,
    timer_interrupts: AtomicU64,
    resched_interrupts: AtomicU64,
    resched_honoured: AtomicU64,
}

impl KCpu {
    const fn new() -> Self {
        Self {
            running: AtomicU64::new(0),
            need_resched: AtomicBool::new(false),
            dispatcher: UnsafeCell::new(arch::KSwitchContext::zeroed()),
            dispatches: AtomicU64::new(0),
            timer_interrupts: AtomicU64::new(0),
            resched_interrupts: AtomicU64::new(0),
            resched_honoured: AtomicU64::new(0),
        }
    }
}

struct FreeList {
    filled: bool,
    len: usize,
    slots: [u8; KTHREAD_CAPACITY],
}

impl FreeList {
    const fn new() -> Self {
        Self {
            filled: false,
            len: 0,
            slots: [0; KTHREAD_CAPACITY],
        }
    }

    /// The list starts full; filled on first use so the pool's initial image is all zeroes.
    fn fill_once(&mut self) {
        if self.filled {
            return;
        }
        self.filled = true;
        // Lowest slot on top, so the first creation takes slot 0.
        for i in 0..KTHREAD_CAPACITY {
            self.slots[i] = (KTHREAD_CAPACITY - 1 - i) as u8;
        }
        self.len = KTHREAD_CAPACITY;
    }

    fn pop(&mut self) -> Option<usize> {
        self.fill_once();
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.slots[self.len] as usize)
    }

    fn push(&mut self, slot: usize) {
        self.fill_once();
        debug_assert!(self.len < KTHREAD_CAPACITY);
        if self.len < KTHREAD_CAPACITY {
            self.slots[self.len] = slot as u8;
            self.len += 1;
        }
    }
}

/// Pool-wide counters (observation only).
#[derive(Default)]
pub(crate) struct KPoolStats {
    created: AtomicU64,
    exhausted: AtomicU64,
    rolled_back: AtomicU64,
    reclaimed: AtomicU64,
    switch_refused: AtomicU64,
    wakes_enqueued: AtomicU64,
    wakes_pending: AtomicU64,
    wakes_stale: AtomicU64,
    early_wakes: AtomicU64,
    irq_refused: AtomicU64,
}

impl KPoolStats {
    const fn new() -> Self {
        Self {
            created: AtomicU64::new(0),
            exhausted: AtomicU64::new(0),
            rolled_back: AtomicU64::new(0),
            reclaimed: AtomicU64::new(0),
            switch_refused: AtomicU64::new(0),
            wakes_enqueued: AtomicU64::new(0),
            wakes_pending: AtomicU64::new(0),
            wakes_stale: AtomicU64::new(0),
            early_wakes: AtomicU64::new(0),
            irq_refused: AtomicU64::new(0),
        }
    }
}

/// A copy of the pool counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KPoolCounters {
    pub created: u64,
    pub exhausted: u64,
    pub rolled_back: u64,
    pub reclaimed: u64,
    pub switch_refused: u64,
    pub wakes_enqueued: u64,
    pub wakes_pending: u64,
    pub wakes_stale: u64,
    pub early_wakes: u64,
    pub irq_refused: u64,
}

/// One CPU's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KCpuCounters {
    pub dispatches: u64,
    pub timer_interrupts: u64,
    pub resched_interrupts: u64,
    pub resched_honoured: u64,
}

/// The production stack storage, reserved at link time (zero-initialised, so it lives in `.bss`).
#[cfg(not(feature = "hosted-dev"))]
#[repr(C, align(4096))]
struct KStacks([[u8; KTHREAD_STACK_BYTES]; KTHREAD_CAPACITY]);

#[cfg(not(feature = "hosted-dev"))]
static mut KSTACKS: KStacks = KStacks([[0; KTHREAD_STACK_BYTES]; KTHREAD_CAPACITY]);

const _: () = assert!(KTHREAD_STACK_BYTES % KTHREAD_STACK_ALIGN == 0);
const _: () = assert!(KTHREAD_CAPACITY <= u8::MAX as usize);

/// The kernel-thread pool. One lives in every [`SharedKernel`] (`SharedKernel::kthreads`).
pub struct KThreadPool {
    slots: [KSlot; KTHREAD_CAPACITY],
    free: SpinLockIrq<FreeList>,
    cpus: [KCpu; MAX_CPUS],
    stats: KPoolStats,
    /// Hosted model only: this pool's own stack storage, allocated on first creation.
    #[cfg(feature = "hosted-dev")]
    hosted_stacks: AtomicUsize,
}

// SAFETY: every field is an atomic, a lock, or a switch context touched only by its home CPU
// with interrupts masked (see `KSlot` and `KCpu`).
unsafe impl Sync for KThreadPool {}
unsafe impl Send for KThreadPool {}

impl core::fmt::Debug for KThreadPool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KThreadPool")
            .field("counters", &self.counters())
            .finish_non_exhaustive()
    }
}

impl Default for KThreadPool {
    fn default() -> Self {
        Self::new()
    }
}

impl KThreadPool {
    pub const fn new() -> Self {
        Self {
            slots: [const { KSlot::new() }; KTHREAD_CAPACITY],
            free: SpinLockIrq::new(FreeList::new()),
            cpus: [const { KCpu::new() }; MAX_CPUS],
            stats: KPoolStats::new(),
            #[cfg(feature = "hosted-dev")]
            hosted_stacks: AtomicUsize::new(0),
        }
    }

    /// The kernel this pool belongs to. A pool exists only as `SharedKernel::kthreads`, so the
    /// owner is found from the pool's own address — never from a pointer cached at some earlier
    /// moment, which a moved `SharedKernel` would leave dangling.
    pub(crate) fn shared(&self) -> &SharedKernel {
        let offset = core::mem::offset_of!(SharedKernel, kthreads);
        // SAFETY: `self` is the `kthreads` field of a live `SharedKernel` (the only place a
        // `KThreadPool` is constructed for use), so stepping back by the field offset yields it.
        unsafe { &*((self as *const Self as *const u8).sub(offset) as *const SharedKernel) }
    }

    // ── stacks ────────────────────────────────────────────────────────────────────────────────

    #[cfg(not(feature = "hosted-dev"))]
    fn stack_base(&self) -> usize {
        core::ptr::addr_of!(KSTACKS) as usize
    }

    #[cfg(feature = "hosted-dev")]
    fn stack_base(&self) -> usize {
        self.hosted_stacks.load(Ordering::Acquire)
    }

    #[cfg(not(feature = "hosted-dev"))]
    fn ensure_stacks(&self) -> bool {
        true
    }

    #[cfg(feature = "hosted-dev")]
    fn ensure_stacks(&self) -> bool {
        if self.hosted_stacks.load(Ordering::Acquire) != 0 {
            return true;
        }
        let _guard = self.free.lock();
        if self.hosted_stacks.load(Ordering::Acquire) != 0 {
            return true;
        }
        let Ok(layout) = core::alloc::Layout::from_size_align(
            KTHREAD_STACK_BYTES * KTHREAD_CAPACITY,
            KTHREAD_STACK_ALIGN,
        ) else {
            return false;
        };
        // SAFETY: non-zero size; freed in `Drop` with the same layout.
        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return false;
        }
        self.hosted_stacks.store(ptr as usize, Ordering::Release);
        true
    }

    /// `[low, high)` of `slot`'s stack.
    pub(crate) fn stack_bounds(&self, slot: usize) -> (usize, usize) {
        let low = self.stack_base() + slot * KTHREAD_STACK_BYTES;
        (low, low + KTHREAD_STACK_BYTES)
    }

    /// The slot whose stack contains `addr`, if any. An address in no stack — a trap stack, a boot
    /// stack, a user address — answers `None`.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn slot_of_stack_addr(&self, addr: usize) -> Option<usize> {
        let base = self.stack_base();
        if base == 0 || addr < base || addr >= base + KTHREAD_STACK_BYTES * KTHREAD_CAPACITY {
            return None;
        }
        Some((addr - base) / KTHREAD_STACK_BYTES)
    }

    fn stack_contains(&self, slot: usize, addr: usize) -> bool {
        let (low, high) = self.stack_bounds(slot);
        addr >= low && addr < high
    }

    /// The deepest byte of `slot`'s stack its current incarnation has written, measured from the
    /// paint laid down at creation. This is OBSERVED depth, not a proven bound.
    pub fn stack_high_water(&self, id: KThreadId) -> usize {
        let (low, _) = self.stack_bounds(id.slot());
        let mut untouched = 0usize;
        while untouched < KTHREAD_STACK_BYTES {
            // SAFETY: inside the slot's own reserved stack.
            let b = unsafe { core::ptr::read_volatile((low + untouched) as *const u8) };
            if b != KTHREAD_STACK_PAINT {
                break;
            }
            untouched += 1;
        }
        KTHREAD_STACK_BYTES - untouched
    }

    // ── observation ───────────────────────────────────────────────────────────────────────────

    /// The state of incarnation `id`, or `None` when it no longer exists (its slot is free or
    /// holds another incarnation).
    pub fn state_of(&self, id: KThreadId) -> Option<KState> {
        let s = &self.slots[id.slot()];
        let state = KState::from_u8(s.state.load(Ordering::Acquire));
        (s.generation.load(Ordering::Acquire) == id.generation() && state != KState::Free)
            .then_some(state)
    }

    /// The kernel thread whose stack is live on `cpu`, if any.
    pub fn resident_on(&self, cpu: CpuId) -> Option<KThreadId> {
        self.cpus
            .get(cpu.0 as usize)
            .and_then(|k| KThreadId::from_raw(k.running.load(Ordering::Acquire)))
    }

    pub fn dispatch_count(&self, id: KThreadId) -> u64 {
        self.slots[id.slot()].dispatches.load(Ordering::Acquire)
    }

    /// The address of `id`'s slot interrupt counter (the witness polls it from a register-patterned
    /// spin; observation only).
    #[cfg(feature = "kthread1-witness")]
    pub(crate) fn interrupt_counter_addr(&self, id: KThreadId) -> *const u64 {
        self.slots[id.slot()].interrupts.as_ptr() as *const u64
    }

    pub fn interrupt_count(&self, id: KThreadId) -> u64 {
        self.slots[id.slot()].interrupts.load(Ordering::Acquire)
    }

    pub fn counters(&self) -> KPoolCounters {
        let s = &self.stats;
        KPoolCounters {
            created: s.created.load(Ordering::Acquire),
            exhausted: s.exhausted.load(Ordering::Acquire),
            rolled_back: s.rolled_back.load(Ordering::Acquire),
            reclaimed: s.reclaimed.load(Ordering::Acquire),
            switch_refused: s.switch_refused.load(Ordering::Acquire),
            wakes_enqueued: s.wakes_enqueued.load(Ordering::Acquire),
            wakes_pending: s.wakes_pending.load(Ordering::Acquire),
            wakes_stale: s.wakes_stale.load(Ordering::Acquire),
            early_wakes: s.early_wakes.load(Ordering::Acquire),
            irq_refused: s.irq_refused.load(Ordering::Acquire),
        }
    }

    pub fn cpu_counters(&self, cpu: CpuId) -> KCpuCounters {
        let Some(k) = self.cpus.get(cpu.0 as usize) else {
            return KCpuCounters::default();
        };
        KCpuCounters {
            dispatches: k.dispatches.load(Ordering::Acquire),
            timer_interrupts: k.timer_interrupts.load(Ordering::Acquire),
            resched_interrupts: k.resched_interrupts.load(Ordering::Acquire),
            resched_honoured: k.resched_honoured.load(Ordering::Acquire),
        }
    }

    /// Free slots right now.
    pub fn free_slots(&self) -> usize {
        let mut f = self.free.lock();
        f.fill_once();
        f.len
    }

    // ── creation ──────────────────────────────────────────────────────────────────────────────

    /// Create a kernel thread pinned to `cpu`, queued there at `priority`.
    ///
    /// Order: a slot is taken from the free list (pool lock), its incarnation is written and its
    /// stack painted and given a first frame (no lock: a Reserved slot is named by no id and in no
    /// queue), and then — under `cpu`'s 1L — it is enqueued and becomes Ready in one step. If the
    /// placement is refused the slot goes straight back to the free list: nothing stays queued or
    /// reserved, and the generation written for it is never valid again.
    pub fn create(
        &self,
        entry: KEntry,
        arg: usize,
        cpu: CpuId,
        priority: TaskPriority,
    ) -> Result<KThreadId, KCreateError> {
        if cpu.0 as usize >= MAX_CPUS {
            return Err(KCreateError::InvalidCpu);
        }
        if !self.ensure_stacks() {
            return Err(KCreateError::NoStorage);
        }
        let Some(slot) = self.free.lock().pop() else {
            self.stats.exhausted.fetch_add(1, Ordering::AcqRel);
            return Err(KCreateError::Exhausted);
        };
        let s = &self.slots[slot];
        // The home and priority are written BEFORE the generation is published, so a holder of
        // this incarnation's id (which exists only after `create` returns) always reads the home
        // that belongs to it.
        s.home.store(cpu.0, Ordering::Release);
        s.priority.store(priority as u8, Ordering::Release);
        s.entry
            .store(entry as *const () as usize, Ordering::Release);
        s.arg.store(arg, Ordering::Release);
        s.wake_pending.store(false, Ordering::Release);
        s.reason.store(KSwitchReason::None as u8, Ordering::Release);
        let mut generation = s.generation.load(Ordering::Acquire).wrapping_add(1);
        if generation == 0 {
            generation = 1;
        }
        s.generation.store(generation, Ordering::Release);
        s.state.store(RESERVED, Ordering::Release);
        let (low, high) = self.stack_bounds(slot);
        // SAFETY: the slot's own reserved stack; no incarnation of this slot is live (Free →
        // Reserved happened under the free list, and the previous one was reclaimed only after
        // its CPU switched away from it).
        unsafe { core::ptr::write_bytes(low as *mut u8, KTHREAD_STACK_PAINT, high - low) };
        // SAFETY: as above — the context belongs to a slot no CPU can enter.
        unsafe {
            arch::prepare_first_entry(&mut *s.ctx.get(), high, self as *const Self as usize, slot)
        };
        let id = KThreadId::encode(slot, generation);
        let placed = self
            .shared()
            .with_cpu_sched_split(cpu, |c| {
                c.enqueue_with_priority(id.tid(), priority).map(|()| {
                    s.state.store(READY, Ordering::Release);
                })
            })
            .and_then(|r| r);
        match placed {
            Ok(()) => {
                crate::arch::hal::notify_enqueued(cpu);
                self.stats.created.fetch_add(1, Ordering::AcqRel);
                Ok(id)
            }
            Err(e) => {
                s.state.store(FREE, Ordering::Release);
                self.free.lock().push(slot);
                self.stats.rolled_back.fetch_add(1, Ordering::AcqRel);
                Err(KCreateError::Placement(e))
            }
        }
    }

    // ── dispatch (called by the drain owner only) ─────────────────────────────────────────────

    /// May `cpu` dispatch `tid` now? Called by the selection owner's acceptance filter, under
    /// `cpu`'s 1L. Read-only.
    pub(crate) fn admit_dispatch(&self, cpu: CpuId, tid: ThreadId) -> bool {
        let Some(id) = KThreadId::from_raw(tid.0) else {
            return false;
        };
        let Some(k) = self.cpus.get(cpu.0 as usize) else {
            return false;
        };
        let s = &self.slots[id.slot()];
        s.state.load(Ordering::Acquire) == READY
            && s.generation.load(Ordering::Acquire) == id.generation()
            && s.home.load(Ordering::Acquire) == cpu.0
            && k.running.load(Ordering::Acquire) == 0
    }

    /// Run the kernel thread the selection owner just made current on `cpu`, until it next leaves
    /// the CPU. Called with interrupts masked by the drain owner, which holds no lock.
    ///
    /// 1. Under `cpu`'s 1L the thread is confirmed current and admissible and becomes Running.
    /// 2. The CPU leaves any user root (`leave_user_root`): a kernel thread runs under the
    ///    kernel's own address space and holds no user root, so residency bookkeeping stays exact.
    /// 3. The residency record names the thread, the CPU's vector base is pointed at the
    ///    kernel-thread vectors, and the dispatcher switches to it.
    /// 4. When it switches back, the vectors are restored and THEN the residency record is cleared:
    ///    only now is its stack not live. An exited thread is reclaimed here, never by itself.
    pub(crate) fn dispatch_selected(&self, cpu: CpuId, tid: ThreadId) -> KSwitchReason {
        let shared = self.shared();
        let Some(id) = KThreadId::from_raw(tid.0) else {
            panic!(
                "KTHREAD_DISPATCH_INVARIANT cpu={} tid={:#x} reason=not_kthread",
                cpu.0, tid.0
            );
        };
        let ci = cpu.0 as usize;
        if ci >= MAX_CPUS || arch::irqs_enabled() {
            panic!(
                "KTHREAD_DISPATCH_INVARIANT cpu={} tid={:#x} reason=unmasked_or_cpu",
                cpu.0, tid.0
            );
        }
        let s = &self.slots[id.slot()];
        let marked = shared
            .with_cpu_sched_split(cpu, |c| {
                let ok = c.current_tid() == Some(tid) && self.admit_dispatch(cpu, tid);
                if ok {
                    s.state.store(RUNNING, Ordering::Release);
                    c.reset_quantum();
                }
                ok
            })
            .unwrap_or(false);
        if !marked {
            panic!(
                "KTHREAD_DISPATCH_INVARIANT cpu={} tid={:#x} reason=not_current_or_inadmissible",
                cpu.0, tid.0
            );
        }
        if !crate::arch::hal::leave_user_root(cpu) {
            panic!(
                "KTHREAD_DISPATCH_INVARIANT cpu={} tid={:#x} reason=kernel_root",
                cpu.0, tid.0
            );
        }
        let k = &self.cpus[ci];
        k.running.store(tid.0, Ordering::Release);
        k.dispatches.fetch_add(1, Ordering::AcqRel);
        s.dispatches.fetch_add(1, Ordering::AcqRel);
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_dispatch(cpu, id);
        arch::before_kthread_dispatch(cpu);
        arch::enter_kthread_vectors(cpu);
        // SAFETY: interrupts are masked; the dispatcher context is this CPU's own, and the slot's
        // context was prepared by `create` or saved by the thread's last switch on this CPU.
        unsafe { arch::switch(k.dispatcher.get(), s.ctx.get()) };
        arch::leave_kthread_vectors(cpu);
        k.running.store(0, Ordering::Release);
        let reason =
            KSwitchReason::from_u8(s.reason.swap(KSwitchReason::None as u8, Ordering::AcqRel));
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_switched_out(
            cpu,
            id,
            reason,
            k.running.load(Ordering::Acquire),
        );
        if reason == KSwitchReason::Exit {
            self.reclaim(cpu, id);
        }
        reason
    }

    /// Return an exited thread's slot to the free list. Only after its CPU switched away (the
    /// residency record is clear) and it is in no queue and not current.
    fn reclaim(&self, cpu: CpuId, id: KThreadId) {
        let s = &self.slots[id.slot()];
        let ok = self
            .shared()
            .with_cpu_sched_split(cpu, |c| {
                let ok = s.state.load(Ordering::Acquire) == EXITED
                    && s.generation.load(Ordering::Acquire) == id.generation()
                    && !c.contains_or_current(id.tid())
                    && self.cpus[cpu.0 as usize].running.load(Ordering::Acquire) != id.raw();
                if ok {
                    s.state.store(FREE, Ordering::Release);
                }
                ok
            })
            .unwrap_or(false);
        if !ok {
            panic!(
                "KTHREAD_RECLAIM_INVARIANT cpu={} tid={:#x}",
                cpu.0,
                id.raw()
            );
        }
        // Witness: recorded BEFORE the slot is published, so no reuse can precede it.
        #[cfg(feature = "kthread1-witness")]
        crate::kernel::kthread_witness::note_reclaim(cpu, id, self.free_slots());
        self.free.lock().push(id.slot());
        self.stats.reclaimed.fetch_add(1, Ordering::AcqRel);
    }

    // ── wake ──────────────────────────────────────────────────────────────────────────────────

    /// Wake incarnation `id`. A Blocked thread is enqueued on its home CPU under that CPU's 1L
    /// (and the CPU is notified after the lock is released); one that has not parked yet gets a
    /// pending wake its park consumes; any other incarnation is stale and untouched.
    ///
    /// KTHREAD1-ACCEPTANCE §1: a wake is never refused. The run queue reserves room for every
    /// kernel thread (`scheduler::MAX_QUEUED_PER_PRIORITY`), so the enqueue cannot fail for
    /// capacity, and a live incarnation's home CPU was online and dispatching when it was created
    /// and stays so (no CPU is taken offline or made wake-only after bring-up). An enqueue refusal
    /// for a Blocked live incarnation is therefore a broken invariant, and it stops the kernel
    /// here rather than leaving a thread Blocked with nobody responsible for waking it.
    pub fn wake(&self, id: KThreadId) -> KWake {
        let s = &self.slots[id.slot()];
        let home = CpuId(s.home.load(Ordering::Acquire));
        let outcome = self
            .shared()
            .with_cpu_sched_split(home, |c| {
                if s.generation.load(Ordering::Acquire) != id.generation() {
                    return Ok(KWake::Stale);
                }
                match s.state.load(Ordering::Acquire) {
                    BLOCKED => match c.enqueue_with_priority(id.tid(), s.priority()) {
                        Ok(()) => {
                            s.state.store(READY, Ordering::Release);
                            Ok(KWake::Enqueued)
                        }
                        Err(e) => Err(e),
                    },
                    RUNNING | READY => {
                        s.wake_pending.store(true, Ordering::Release);
                        Ok(KWake::Pending)
                    }
                    _ => Ok(KWake::Stale),
                }
            })
            .and_then(|r| r);
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => panic!(
                "KTHREAD_WAKE_INVARIANT cpu={} kthread={:#x} refused={:?}",
                home.0,
                id.raw(),
                e
            ),
        };
        match outcome {
            KWake::Enqueued => {
                crate::arch::hal::notify_enqueued(home);
                self.stats.wakes_enqueued.fetch_add(1, Ordering::AcqRel);
            }
            KWake::Pending => {
                self.stats.wakes_pending.fetch_add(1, Ordering::AcqRel);
            }
            KWake::Stale => {
                self.stats.wakes_stale.fetch_add(1, Ordering::AcqRel);
            }
        }
        outcome
    }

    // ── kernel-origin interrupts ──────────────────────────────────────────────────────────────

    /// Accept an interrupt taken while a kernel thread ran, or say why not. `frame_addr` is where
    /// the entry saved the interrupted state (on the interrupted stack); `hardware_cpu` is the CPU
    /// the hardware names. The thread, its incarnation, its stack, the residency record and the
    /// hardware CPU must all agree. Never logs, never allocates.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn authenticate_interrupt(
        &self,
        frame_addr: usize,
        hardware_cpu: CpuId,
    ) -> Result<(CpuId, KThreadId), KIrqRefusal> {
        let refused = |r| {
            self.stats.irq_refused.fetch_add(1, Ordering::AcqRel);
            Err(r)
        };
        let Some(slot) = self.slot_of_stack_addr(frame_addr) else {
            return refused(KIrqRefusal::NotKernelThreadStack);
        };
        let s = &self.slots[slot];
        let cpu = CpuId(s.home.load(Ordering::Acquire));
        if cpu != hardware_cpu || cpu.0 as usize >= MAX_CPUS {
            return refused(KIrqRefusal::CpuMismatch);
        }
        let Some(id) =
            KThreadId::from_raw(self.cpus[cpu.0 as usize].running.load(Ordering::Acquire))
        else {
            return refused(KIrqRefusal::NotRunning);
        };
        if id.slot() != slot {
            return refused(KIrqRefusal::StackMismatch);
        }
        if s.generation.load(Ordering::Acquire) != id.generation()
            || s.state.load(Ordering::Acquire) != RUNNING
        {
            return refused(KIrqRefusal::Stale);
        }
        s.interrupts.fetch_add(1, Ordering::AcqRel);
        Ok((cpu, id))
    }

    /// The timer's obligations for an interrupt that landed on a running kernel thread: the same
    /// tick, acknowledgement and re-arm the split timer route performs, and — instead of switching
    /// — a pending reschedule when the quantum expired. Takes only `cpu`'s 1L (interrupt-safe).
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn service_timer_interrupt(&self, cpu: CpuId) -> bool {
        let shared = self.shared();
        // The AP-scheduling witness's own tick accounting, exactly as the split timer route
        // brackets its tick (default-off; neither hook logs or locks).
        #[cfg(feature = "apsched-witness")]
        crate::kernel::apsched_witness::note_timer_irq(cpu);
        let preempting = matches!(
            shared.scheduler_tick_split_mut(cpu),
            crate::runtime::SchedulerTickOutcome::Preempt { .. }
        );
        #[cfg(feature = "apsched-witness")]
        crate::kernel::apsched_witness::note_timer_tick_done(cpu);
        crate::arch::hal_adapters::acknowledge_interrupt(cpu, 0);
        crate::arch::hal_adapters::program_timer_deadline(
            cpu,
            crate::arch::platform_constants::BOOTSTRAP_TIMER_DEADLINE_TICKS,
        );
        let k = &self.cpus[cpu.0 as usize];
        k.timer_interrupts.fetch_add(1, Ordering::AcqRel);
        if preempting {
            k.need_resched.store(true, Ordering::Release);
        }
        preempting
    }

    /// A reschedule IPI landed on a running kernel thread: the remote enqueue it announces is
    /// already committed on this CPU's queue, so the thread owes a scheduling point.
    #[cfg_attr(feature = "hosted-dev", allow(dead_code))]
    pub(crate) fn note_resched_interrupt(&self, cpu: CpuId, announced: bool) {
        let k = &self.cpus[cpu.0 as usize];
        k.resched_interrupts.fetch_add(1, Ordering::AcqRel);
        if announced {
            k.need_resched.store(true, Ordering::Release);
        }
    }

    /// Is a reschedule pending on `cpu` (observation only)?
    pub fn resched_pending(&self, cpu: CpuId) -> bool {
        self.cpus
            .get(cpu.0 as usize)
            .is_some_and(|k| k.need_resched.load(Ordering::Acquire))
    }
}

#[cfg(feature = "hosted-dev")]
impl Drop for KThreadPool {
    fn drop(&mut self) {
        let ptr = self.hosted_stacks.load(Ordering::Acquire);
        if ptr != 0
            && let Ok(layout) = core::alloc::Layout::from_size_align(
                KTHREAD_STACK_BYTES * KTHREAD_CAPACITY,
                KTHREAD_STACK_ALIGN,
            )
        {
            // SAFETY: allocated in `ensure_stacks` with this layout; no thread of this pool can be
            // running once the pool is being dropped.
            unsafe { alloc::alloc::dealloc(ptr as *mut u8, layout) };
        }
    }
}

/// First Rust code of every kernel thread, entered from the architecture trampoline on the
/// thread's own stack with interrupts masked. Enables interrupts, runs the body, and exits.
#[unsafe(no_mangle)]
extern "C" fn yarm_kthread_first_entry(pool: usize, slot: usize) -> ! {
    // SAFETY: `create` put this pool's address and this slot's index in the first frame.
    let pool = unsafe { &*(pool as *const KThreadPool) };
    let s = &pool.slots[slot];
    let ctx = KThreadCtx {
        pool,
        id: KThreadId::encode(slot, s.generation.load(Ordering::Acquire)),
        home: CpuId(s.home.load(Ordering::Acquire)),
        masked_depth: Cell::new(0),
        _not_send: PhantomData,
    };
    // SAFETY: `create` stored a `KEntry` here.
    let entry: KEntry =
        unsafe { core::mem::transmute::<usize, KEntry>(s.entry.load(Ordering::Acquire)) };
    let arg = s.arg.load(Ordering::Acquire);
    arch::irq_enable();
    entry(&ctx, arg);
    ctx.exit()
}

/// A running kernel thread's handle on itself. Exists only on the thread's own stack and cannot
/// leave it (`!Send`); every scheduling operation goes through it and authenticates against it.
pub struct KThreadCtx {
    pool: &'static KThreadPool,
    id: KThreadId,
    home: CpuId,
    masked_depth: Cell<u32>,
    _not_send: PhantomData<*const ()>,
}

/// An interrupt-masked section of a kernel thread. Every kernel-owner call a thread makes runs
/// inside one; no scheduling point is allowed while one is open.
pub struct KMasked<'a> {
    ctx: &'a KThreadCtx,
    was_enabled: bool,
}

impl Drop for KMasked<'_> {
    fn drop(&mut self) {
        self.ctx.masked_depth.set(self.ctx.masked_depth.get() - 1);
        arch::irq_restore(self.was_enabled);
    }
}

impl KThreadCtx {
    pub fn id(&self) -> KThreadId {
        self.id
    }

    pub fn home(&self) -> CpuId {
        self.home
    }

    pub fn pool(&self) -> &'static KThreadPool {
        self.pool
    }

    pub fn shared(&self) -> &'static SharedKernel {
        self.pool.shared()
    }

    /// Open a masked section.
    pub fn masked(&self) -> KMasked<'_> {
        let was_enabled = arch::irq_disable_save();
        self.masked_depth.set(self.masked_depth.get() + 1);
        KMasked {
            ctx: self,
            was_enabled,
        }
    }

    /// The switch-safety check every scheduling point makes first. A refusal changes nothing.
    pub fn switch_check(&self, op: &'static str) -> Result<(), KSwitchRefusal> {
        let s = &self.pool.slots[self.id.slot()];
        let refusal = if !arch::irqs_enabled() {
            Some(KSwitchRefusal::IrqMasked)
        } else if self.masked_depth.get() != 0 {
            Some(KSwitchRefusal::GuardHeld)
        } else if self.pool.cpus[self.home.0 as usize]
            .running
            .load(Ordering::Acquire)
            != self.id.raw()
        {
            Some(KSwitchRefusal::NotResident)
        } else if !self.pool.stack_contains(self.id.slot(), arch::current_sp()) {
            Some(KSwitchRefusal::WrongStack)
        } else if s.generation.load(Ordering::Acquire) != self.id.generation()
            || s.state.load(Ordering::Acquire) != RUNNING
        {
            Some(KSwitchRefusal::Stale)
        } else {
            None
        };
        match refusal {
            None => Ok(()),
            Some(r) => {
                self.pool
                    .stats
                    .switch_refused
                    .fetch_add(1, Ordering::AcqRel);
                crate::yarm_log!(
                    "KTHREAD_SWITCH_REFUSED cpu={} kthread={:#x} op={} reason={}",
                    self.home.0,
                    self.id.raw(),
                    op,
                    r.marker()
                );
                Err(r)
            }
        }
    }

    fn switch_to_dispatcher(&self) {
        let s = &self.pool.slots[self.id.slot()];
        let k = &self.pool.cpus[self.home.0 as usize];
        // SAFETY: interrupts are masked (every caller masked first), this is the home CPU, and the
        // dispatcher context was saved by the `switch` that entered this thread.
        unsafe { arch::switch(s.ctx.get(), k.dispatcher.get()) };
    }

    /// Give the CPU to the next ready task of this CPU (possibly this thread again, if nothing
    /// else is ready). Returns when the thread is next dispatched.
    pub fn yield_now(&self) -> Result<(), KSwitchRefusal> {
        self.switch_check("yield")?;
        self.yield_checked()
    }

    fn yield_checked(&self) -> Result<(), KSwitchRefusal> {
        let s = &self.pool.slots[self.id.slot()];
        let was = arch::irq_disable_save();
        let tid = self.id.tid();
        let requeued = self
            .shared()
            .with_cpu_sched_split(self.home, |c| {
                if c.current_tid() != Some(tid) {
                    return false;
                }
                if c.preempt_reenqueue_only() == Some(tid) {
                    s.state.store(READY, Ordering::Release);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !requeued {
            arch::irq_restore(was);
            self.pool
                .stats
                .switch_refused
                .fetch_add(1, Ordering::AcqRel);
            return Err(KSwitchRefusal::Scheduler);
        }
        s.reason
            .store(KSwitchReason::Yield as u8, Ordering::Release);
        self.switch_to_dispatcher();
        arch::irq_restore(was);
        Ok(())
    }

    /// The explicit scheduling point for long work: yields if an interrupt left a reschedule
    /// pending for this CPU (quantum expiry, or a remote enqueue's reschedule IPI), otherwise
    /// returns at once. Returns whether it yielded.
    pub fn sched_point(&self) -> Result<bool, KSwitchRefusal> {
        self.switch_check("sched_point")?;
        let k = &self.pool.cpus[self.home.0 as usize];
        if !k.need_resched.swap(false, Ordering::AcqRel) {
            return Ok(false);
        }
        k.resched_honoured.fetch_add(1, Ordering::AcqRel);
        self.yield_checked().map(|()| true)
    }

    /// Park until woken. A wake that arrived since the caller registered (`wake_pending`) makes
    /// this return at once without blocking; that check and the block are one 1L section, so no
    /// wake falls between them. Returns whether the thread actually blocked.
    fn park(&self) -> Result<bool, KSwitchRefusal> {
        self.switch_check("block")?;
        let s = &self.pool.slots[self.id.slot()];
        let was = arch::irq_disable_save();
        let tid = self.id.tid();
        // 0 = woken early, 1 = blocked, 2 = refused.
        let outcome = self
            .shared()
            .with_cpu_sched_split(self.home, |c| {
                if s.wake_pending.swap(false, Ordering::AcqRel) {
                    return 0u8;
                }
                if c.block_current_exact(tid).is_some() {
                    s.state.store(BLOCKED, Ordering::Release);
                    1
                } else {
                    2
                }
            })
            .unwrap_or(2);
        match outcome {
            0 => {
                arch::irq_restore(was);
                self.pool.stats.early_wakes.fetch_add(1, Ordering::AcqRel);
                Ok(false)
            }
            1 => {
                s.reason
                    .store(KSwitchReason::Block as u8, Ordering::Release);
                self.switch_to_dispatcher();
                arch::irq_restore(was);
                Ok(true)
            }
            _ => {
                arch::irq_restore(was);
                Err(KSwitchRefusal::Scheduler)
            }
        }
    }

    /// End this thread. Its slot is reclaimed by the dispatcher after the switch away; the thread
    /// never frees the stack it is running on.
    pub fn exit(&self) -> ! {
        if let Err(r) = self.switch_check("exit") {
            panic!(
                "KTHREAD_EXIT_REFUSED cpu={} kthread={:#x} reason={}",
                self.home.0,
                self.id.raw(),
                r.marker()
            );
        }
        let s = &self.pool.slots[self.id.slot()];
        let _was = arch::irq_disable_save();
        let tid = self.id.tid();
        let exited = self
            .shared()
            .with_cpu_sched_split(self.home, |c| {
                if c.block_current_exact(tid).is_some() {
                    s.state.store(EXITED, Ordering::Release);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !exited {
            panic!(
                "KTHREAD_EXIT_INVARIANT cpu={} kthread={:#x}",
                self.home.0,
                self.id.raw()
            );
        }
        s.reason.store(KSwitchReason::Exit as u8, Ordering::Release);
        self.switch_to_dispatcher();
        panic!(
            "KTHREAD_RESUMED_AFTER_EXIT cpu={} kthread={:#x}",
            self.home.0,
            self.id.raw()
        );
    }
}

// ── the internal wait owner ───────────────────────────────────────────────────────────────────

/// How many kernel threads one wait queue can hold registered at once.
pub const KWAIT_CAPACITY: usize = 8;

/// Why a wait failed. A failed wait leaves no registration behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KWaitError {
    /// The queue's registrations are full.
    Full,
    /// The scheduling point was refused.
    Refused(KSwitchRefusal),
}

struct KWaitInner {
    len: usize,
    waiters: [u64; KWAIT_CAPACITY],
}

impl KWaitInner {
    fn position(&self, raw: u64) -> Option<usize> {
        self.waiters[..self.len].iter().position(|&w| w == raw)
    }

    fn remove(&mut self, raw: u64) {
        if let Some(i) = self.position(raw) {
            self.waiters.copy_within(i + 1..self.len, i);
            self.len -= 1;
        }
    }
}

/// A condition kernel threads wait on.
///
/// Protocol: a waiter evaluates its condition UNDER this queue's lock and registers before
/// releasing it; a waker changes the condition's inputs first and then calls a `wake_*`, which
/// takes the same lock to collect registrations. So a wake can never fall between a failed check
/// and the registration (no ready-flag-then-wait gap). A wake that reaches a registered thread
/// before it has parked is kept as `wake_pending` under the home CPU's 1L and consumed by the park,
/// and every return from a park re-evaluates the condition, so duplicate, early and stale wakes are
/// all harmless.
pub struct KWaitQueue {
    inner: SpinLockIrq<KWaitInner>,
}

impl Default for KWaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl KWaitQueue {
    pub const fn new() -> Self {
        Self {
            inner: SpinLockIrq::new(KWaitInner {
                len: 0,
                waiters: [0; KWAIT_CAPACITY],
            }),
        }
    }

    /// Wait until `cond` holds. Returns how many times the thread actually blocked.
    pub fn wait_until(&self, ctx: &KThreadCtx, cond: impl Fn() -> bool) -> Result<u32, KWaitError> {
        let me = ctx.id.raw();
        let mut blocked = 0u32;
        loop {
            // Refuse before registering, so a refused wait leaves nothing behind.
            ctx.switch_check("wait").map_err(KWaitError::Refused)?;
            {
                let mut q = self.inner.lock();
                if cond() {
                    q.remove(me);
                    return Ok(blocked);
                }
                if q.position(me).is_none() {
                    if q.len == KWAIT_CAPACITY {
                        return Err(KWaitError::Full);
                    }
                    let len = q.len;
                    q.waiters[len] = me;
                    q.len += 1;
                }
            }
            match ctx.park() {
                Ok(true) => blocked += 1,
                Ok(false) => {}
                Err(r) => {
                    self.inner.lock().remove(me);
                    return Err(KWaitError::Refused(r));
                }
            }
        }
    }

    /// Wake every registered thread. Returns the count of threads made ready or given a pending
    /// wake. Taking the registrations is safe because no wake can be refused ([`KThreadPool::wake`]):
    /// each one is either placed on its home CPU's queue, kept pending for a park that has not
    /// happened yet, or names an incarnation that no longer exists. A woken thread re-evaluates its
    /// condition and registers again if it still does not hold.
    pub fn wake_all(&self, pool: &KThreadPool) -> usize {
        let mut taken = [0u64; KWAIT_CAPACITY];
        let n = {
            let mut q = self.inner.lock();
            let n = q.len;
            taken[..n].copy_from_slice(&q.waiters[..n]);
            q.len = 0;
            n
        };
        let mut woken = 0;
        for &raw in &taken[..n] {
            if let Some(id) = KThreadId::from_raw(raw)
                && matches!(pool.wake(id), KWake::Enqueued | KWake::Pending)
            {
                woken += 1;
            }
        }
        woken
    }

    /// Registered threads right now (observation only).
    pub fn registered(&self) -> usize {
        self.inner.lock().len
    }
}

#[cfg(test)]
#[path = "kthread_tests.rs"]
mod tests;
