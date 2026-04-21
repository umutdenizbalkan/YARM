// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::ipc::Message;
use super::process_abi::{
    PROC_OP_EXIT, PROC_OP_GETPID, PROC_OP_GETPPID, PROC_OP_SPAWN_V2, PROC_OP_SPAWN_V3,
    PROC_OP_SPAWN_V4, PROC_OP_WAITPID_V2, SpawnV2Args, SpawnV3Args, SpawnV4Args, WaitPidV2Args,
    WaitPidV2Reply,
};
use super::task::{TaskClass, ThreadGroupId};
use crate::services::common::service::RequestResponseService;

const MAX_PROCESSES: usize = 64;
const MAX_THREADS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessManagerError {
    Malformed,
    Unsupported,
    TableFull,
    UnknownProcess,
    InvalidTransport,
    PermissionDenied,
    WouldBlock,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfImageInfo {
    pub entry: u64,
    pub image_id: u64,
}

impl ElfImageInfo {
    const EI_CLASS: usize = 4;
    const EI_DATA: usize = 5;
    const EI_OSABI: usize = 7;
    const ELFCLASS64: u8 = 2;
    const ELFDATA2LSB: u8 = 1;
    const ELFDATA2MSB: u8 = 2;
    const ELFOSABI_SYSV: u8 = 0;
    const ELFOSABI_GNU: u8 = 3;
    const ELFOSABI_STANDALONE: u8 = 255;
    const ET_EXEC: u16 = 2;
    const ET_DYN: u16 = 3;
    const PT_LOAD: u32 = 1;
    const ELF64_EHDR_SIZE: usize = 64;
    const ELF64_PHDR_SIZE: usize = 56;

    fn read_u16(image: &[u8], offset: usize, big_endian: bool) -> Result<u16, ProcessManagerError> {
        let end = offset
            .checked_add(2)
            .ok_or(ProcessManagerError::Malformed)?;
        let bytes = image
            .get(offset..end)
            .ok_or(ProcessManagerError::Malformed)?;
        let mut raw = [0u8; 2];
        raw.copy_from_slice(bytes);
        Ok(if big_endian {
            u16::from_be_bytes(raw)
        } else {
            u16::from_le_bytes(raw)
        })
    }

    fn read_u32(image: &[u8], offset: usize, big_endian: bool) -> Result<u32, ProcessManagerError> {
        let end = offset
            .checked_add(4)
            .ok_or(ProcessManagerError::Malformed)?;
        let bytes = image
            .get(offset..end)
            .ok_or(ProcessManagerError::Malformed)?;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(bytes);
        Ok(if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        })
    }

    fn read_u64(image: &[u8], offset: usize, big_endian: bool) -> Result<u64, ProcessManagerError> {
        let end = offset
            .checked_add(8)
            .ok_or(ProcessManagerError::Malformed)?;
        let bytes = image
            .get(offset..end)
            .ok_or(ProcessManagerError::Malformed)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        Ok(if big_endian {
            u64::from_be_bytes(raw)
        } else {
            u64::from_le_bytes(raw)
        })
    }

    fn expected_machine() -> u16 {
        #[cfg(target_arch = "x86_64")]
        {
            return 0x3E;
        }
        #[cfg(target_arch = "riscv64")]
        {
            return 0xF3;
        }
        #[cfg(target_arch = "aarch64")]
        {
            return 0xB7;
        }
        #[allow(unreachable_code)]
        0
    }

    pub fn parse(image_id: u64, image: &[u8]) -> Result<Self, ProcessManagerError> {
        if image.len() < Self::ELF64_EHDR_SIZE {
            return Err(ProcessManagerError::Malformed);
        }
        if &image[..4] != b"\x7FELF" {
            return Err(ProcessManagerError::Malformed);
        }
        if image[Self::EI_CLASS] != Self::ELFCLASS64 {
            return Err(ProcessManagerError::Unsupported);
        }
        let data = image[Self::EI_DATA];
        let big_endian = match data {
            Self::ELFDATA2LSB => false,
            Self::ELFDATA2MSB => true,
            _ => return Err(ProcessManagerError::Unsupported),
        };
        match image[Self::EI_OSABI] {
            Self::ELFOSABI_SYSV | Self::ELFOSABI_GNU | Self::ELFOSABI_STANDALONE => {}
            _ => return Err(ProcessManagerError::Unsupported),
        }

        let e_type = Self::read_u16(image, 16, big_endian)?;
        if e_type != Self::ET_EXEC && e_type != Self::ET_DYN {
            return Err(ProcessManagerError::Unsupported);
        }
        let e_machine = Self::read_u16(image, 18, big_endian)?;
        if e_machine != Self::expected_machine() {
            return Err(ProcessManagerError::Unsupported);
        }

        let entry = Self::read_u64(image, 24, big_endian)?;
        let phoff = Self::read_u64(image, 32, big_endian)? as usize;
        let phentsize = Self::read_u16(image, 54, big_endian)? as usize;
        let phnum = Self::read_u16(image, 56, big_endian)? as usize;
        if phnum == 0 || phentsize < Self::ELF64_PHDR_SIZE {
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

        let mut load_segments = 0usize;
        for idx in 0..phnum {
            let base = phoff
                .checked_add(
                    idx.checked_mul(phentsize)
                        .ok_or(ProcessManagerError::Malformed)?,
                )
                .ok_or(ProcessManagerError::Malformed)?;
            let p_type = Self::read_u32(image, base, big_endian)?;
            if p_type != Self::PT_LOAD {
                continue;
            }
            load_segments = load_segments.saturating_add(1);
            let p_offset = Self::read_u64(image, base + 8, big_endian)? as usize;
            let _p_vaddr = Self::read_u64(image, base + 16, big_endian)?;
            let _p_paddr = Self::read_u64(image, base + 24, big_endian)?;
            let p_filesz = Self::read_u64(image, base + 32, big_endian)? as usize;
            let p_memsz = Self::read_u64(image, base + 40, big_endian)? as usize;
            let _p_flags = Self::read_u32(image, base + 4, big_endian)?;
            if p_filesz > p_memsz {
                return Err(ProcessManagerError::Malformed);
            }
            let seg_end = p_offset
                .checked_add(p_filesz)
                .ok_or(ProcessManagerError::Malformed)?;
            if seg_end > image.len() {
                return Err(ProcessManagerError::Malformed);
            }
        }
        if load_segments == 0 {
            return Err(ProcessManagerError::Malformed);
        }

        Ok(Self { entry, image_id })
    }
}

pub struct WaitPidV2Result {
    pub waited_pid: ProcessId,
    pub exit_code: u64,
}

impl WaitPidV2Result {
    pub const fn encode(self) -> [u8; 16] {
        WaitPidV2Reply::new(self.waited_pid.0, self.exit_code).encode()
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProcessManagerError> {
        let args = WaitPidV2Reply::decode(payload).map_err(|_| ProcessManagerError::Malformed)?;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRecord {
    pid: ProcessId,
    parent_pid: ProcessId,
    exited: bool,
    exit_code: u64,
    image_id: u64,
    entry: u64,
    requested_cnode_slots: Option<usize>,
    requested_task_class: Option<TaskClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThreadIdentityRecord {
    tid: u64,
    pid: ProcessId,
    thread_group_id: ThreadGroupId,
}

#[derive(Debug)]
pub struct ProcessManager {
    next_pid: ProcessId,
    table: [Option<ProcessRecord>; MAX_PROCESSES],
    threads: [Option<ThreadIdentityRecord>; MAX_THREADS],
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub const fn new() -> Self {
        Self {
            next_pid: ProcessId(1000),
            table: [None; MAX_PROCESSES],
            threads: [None; MAX_THREADS],
        }
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
            _ => Err(ProcessManagerError::Unsupported),
        }
    }

    fn u64_reply(opcode: u16, value: u64) -> Result<Message, ProcessManagerError> {
        Message::with_header(0, opcode, 0, None, &value.to_le_bytes())
            .map_err(|_| ProcessManagerError::Malformed)
    }

    pub fn register_thread_identity(
        &mut self,
        pid: ProcessId,
        tid: u64,
        thread_group_id: ThreadGroupId,
    ) -> Result<(), ProcessManagerError> {
        if self
            .threads
            .iter()
            .flatten()
            .any(|record| record.tid == tid)
        {
            return Ok(());
        }
        let slot = self
            .threads
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ProcessManagerError::TableFull)?;
        *slot = Some(ThreadIdentityRecord {
            tid,
            pid,
            thread_group_id,
        });
        Ok(())
    }

    fn thread_identity(&self, tid: u64) -> Option<ThreadIdentityRecord> {
        self.threads
            .iter()
            .flatten()
            .find(|record| record.tid == tid)
            .copied()
    }

    fn process_id_for_tid(&self, caller_tid: u64) -> ProcessId {
        self.thread_identity(caller_tid)
            .map(|record| record.pid)
            .unwrap_or(ProcessId(caller_tid))
    }

    pub fn spawn_from_elf_image(
        &mut self,
        parent_pid: ProcessId,
        image_id: u64,
        image: &[u8],
        requested_cnode_slots: Option<usize>,
        requested_task_class: Option<TaskClass>,
    ) -> Result<(ProcessId, ElfImageInfo), ProcessManagerError> {
        let info = ElfImageInfo::parse(image_id, image)?;
        let pid = self.alloc_process(parent_pid)?;
        if let Some(record) = self
            .table
            .iter_mut()
            .flatten()
            .find(|record| record.pid == pid)
        {
            record.image_id = info.image_id;
            record.entry = info.entry;
            record.requested_cnode_slots = requested_cnode_slots;
            record.requested_task_class = requested_task_class;
        }
        Ok((pid, info))
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

        // Single PT_LOAD segment.
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

    fn alloc_process(&mut self, parent_pid: ProcessId) -> Result<ProcessId, ProcessManagerError> {
        let pid = self.next_pid;
        self.next_pid = ProcessId(self.next_pid.0.saturating_add(1));
        if let Some(slot) = self.table.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ProcessRecord {
                pid,
                parent_pid,
                exited: false,
                exit_code: 0,
                image_id: 0,
                entry: 0,
                requested_cnode_slots: None,
                requested_task_class: None,
            });
            self.register_thread_identity(pid, pid.0, ThreadGroupId(pid.0))?;
            Ok(pid)
        } else {
            Err(ProcessManagerError::TableFull)
        }
    }

    pub fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessManagerError> {
        let record = self
            .table
            .iter_mut()
            .flatten()
            .find(|record| record.pid == pid)
            .ok_or(ProcessManagerError::UnknownProcess)?;
        record.exited = true;
        record.exit_code = code;
        Ok(())
    }

    fn mark_exit_for_tid(&mut self, caller_tid: u64, code: u64) -> Result<(), ProcessManagerError> {
        let caller_pid = self.process_id_for_tid(caller_tid);
        if self.mark_exit(caller_pid, code).is_ok() {
            return Ok(());
        }
        let slot = self
            .table
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ProcessManagerError::TableFull)?;
        *slot = Some(ProcessRecord {
            pid: caller_pid,
            parent_pid: ProcessId(caller_pid.0.saturating_sub(1)),
            exited: true,
            exit_code: code,
            image_id: 0,
            entry: 0,
            requested_cnode_slots: None,
            requested_task_class: None,
        });
        self.register_thread_identity(caller_pid, caller_tid, ThreadGroupId(caller_tid))?;
        Ok(())
    }

    fn lookup_parent(&self, pid: ProcessId) -> Option<ProcessId> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.parent_pid)
    }

    fn wait_result(
        &mut self,
        target_pid: ProcessId,
    ) -> Result<WaitPidV2Result, ProcessManagerError> {
        if let Some((idx, record)) = self
            .table
            .iter()
            .enumerate()
            .find_map(|(idx, slot)| slot.map(|record| (idx, record)))
            .filter(|(_, record)| record.pid == target_pid)
        {
            if !record.exited {
                return Err(ProcessManagerError::WouldBlock);
            }
            let result = WaitPidV2Result {
                waited_pid: target_pid,
                exit_code: record.exit_code,
            };
            self.table[idx] = None;
            Ok(result)
        } else {
            Err(ProcessManagerError::UnknownProcess)
        }
    }

    pub fn live_process_count(&self) -> usize {
        self.table.iter().flatten().count()
    }

    pub fn process_image_info(&self, pid: ProcessId) -> Option<ElfImageInfo> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| ElfImageInfo {
                entry: record.entry,
                image_id: record.image_id,
            })
    }

    pub fn process_requested_cnode_slots(&self, pid: ProcessId) -> Option<Option<usize>> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.requested_cnode_slots)
    }

    pub fn process_requested_task_class(&self, pid: ProcessId) -> Option<Option<TaskClass>> {
        self.table
            .iter()
            .flatten()
            .find(|record| record.pid == pid)
            .map(|record| record.requested_task_class)
    }

    pub fn handle_request(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        match Self::parse_request(request)? {
            ProcessRequest::GetPid { caller_tid } => {
                Self::u64_reply(PROC_OP_GETPID, self.process_id_for_tid(caller_tid).0)
            }
            ProcessRequest::GetPpid { caller_tid } => {
                let pid = self.process_id_for_tid(caller_tid);
                Self::u64_reply(
                    PROC_OP_GETPPID,
                    self.lookup_parent(pid)
                        .unwrap_or(ProcessId(pid.0.saturating_sub(1)))
                        .0,
                )
            }
            ProcessRequest::Exit { caller_tid, code } => {
                self.mark_exit_for_tid(caller_tid, code)?;
                Self::u64_reply(PROC_OP_EXIT, 0)
            }
            ProcessRequest::SpawnV2(req) => {
                #[cfg(test)]
                {
                    let image = Self::synthetic_elf_image(req.image_id);
                    let (pid, _) = self.spawn_from_elf_image(
                        req.parent_pid,
                        req.image_id,
                        &image,
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
                    let Some(parent) = self.lookup_parent(req.target_pid) else {
                        return Err(ProcessManagerError::PermissionDenied);
                    };
                    if parent != req.caller_pid {
                        return Err(ProcessManagerError::PermissionDenied);
                    }
                }
                let result = self.wait_result(req.target_pid)?;
                Message::with_header(0, PROC_OP_WAITPID_V2, 0, None, &result.encode())
                    .map_err(|_| ProcessManagerError::Malformed)
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ProcessService {
    manager: ProcessManager,
    handled: usize,
}

impl ProcessService {
    pub const fn new() -> Self {
        Self {
            manager: ProcessManager::new(),
            handled: 0,
        }
    }

    pub const fn handled_count(&self) -> usize {
        self.handled
    }

    pub fn requested_cnode_slots_for_process(&self, pid: u64) -> Option<Option<usize>> {
        self.manager.process_requested_cnode_slots(ProcessId(pid))
    }

    pub fn requested_task_class_for_process(&self, pid: u64) -> Option<Option<TaskClass>> {
        self.manager.process_requested_task_class(ProcessId(pid))
    }

    pub fn mark_exit(&mut self, pid: ProcessId, code: u64) -> Result<(), ProcessManagerError> {
        self.manager.mark_exit(pid, code)
    }

    pub fn handle(&mut self, request: Message) -> Result<Message, ProcessManagerError> {
        let reply = self.manager.handle_request(request)?;
        self.handled = self.handled.saturating_add(1);
        Ok(reply)
    }

    pub fn handle_batch(
        &mut self,
        requests: impl IntoIterator<Item = Message>,
    ) -> Result<usize, ProcessManagerError> {
        for request in requests {
            self.handle(request)?;
        }
        Ok(self.handled)
    }
}

impl RequestResponseService for ProcessService {
    type Error = ProcessManagerError;

    fn service_name(&self) -> &'static str {
        "process_manager"
    }

    fn handle(&mut self, request: Message) -> Result<Message, Self::Error> {
        ProcessService::handle(self, request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yarm_ipc_abi::process_abi::{SpawnV2Args, SpawnV3Args, SpawnV4Args, WaitPidV2Args};

    #[test]
    fn elf_image_info_parser_accepts_minimal_elf64_header() {
        let mut image = ProcessManager::synthetic_elf_image(1);
        image[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        let info = ElfImageInfo::parse(7, &image).expect("elf");
        assert_eq!(info.image_id, 7);
        assert_eq!(info.entry, 0x401000);
    }

    #[test]
    fn spawn_from_elf_image_allocates_pid_and_returns_entry() {
        let mut pm = ProcessManager::new();
        let mut image = ProcessManager::synthetic_elf_image(2);
        image[24..32].copy_from_slice(&0x402000u64.to_le_bytes());

        let (pid, info) = pm
            .spawn_from_elf_image(ProcessId(1), 9, &image, None, None)
            .expect("spawn from image");
        assert!(pid >= ProcessId(1000));
        assert_eq!(info.image_id, 9);
        assert_eq!(info.entry, 0x402000);
    }

    #[test]
    fn process_manager_parses_v2_payloads() {
        let msg = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(7, 99).encode(),
        )
        .expect("msg");
        let req = ProcessManager::parse_request(msg).expect("parse");
        assert_eq!(
            req,
            ProcessRequest::SpawnV2(SpawnV2Request {
                parent_pid: ProcessId(7),
                image_id: 99,
                requested_cnode_slots: None,
                requested_task_class: None,
            })
        );
    }

    #[test]
    fn process_manager_parses_v3_payloads_with_requested_cnode_slots() {
        let msg = Message::with_header(
            0,
            PROC_OP_SPAWN_V3,
            0,
            None,
            &SpawnV3Args::new(7, 99, 64).encode(),
        )
        .expect("msg");
        let req = ProcessManager::parse_request(msg).expect("parse");
        assert_eq!(
            req,
            ProcessRequest::SpawnV2(SpawnV2Request {
                parent_pid: ProcessId(7),
                image_id: 99,
                requested_cnode_slots: Some(64),
                requested_task_class: None,
            })
        );
    }

    #[test]
    fn process_manager_parses_v4_payloads_with_requested_slots_and_class() {
        let msg = Message::with_header(
            0,
            PROC_OP_SPAWN_V4,
            0,
            None,
            &SpawnV4Args::new(7, 99, 64, 2).encode(),
        )
        .expect("msg");
        let req = ProcessManager::parse_request(msg).expect("parse");
        assert_eq!(
            req,
            ProcessRequest::SpawnV2(SpawnV2Request {
                parent_pid: ProcessId(7),
                image_id: 99,
                requested_cnode_slots: Some(64),
                requested_task_class: Some(TaskClass::SystemServer),
            })
        );
    }

    #[test]
    fn process_manager_v3_spawn_records_requested_cnode_slots() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V3,
            0,
            None,
            &SpawnV3Args::new(1, 2, 96).encode(),
        )
        .expect("msg");
        let spawn_reply = pm.handle_request(spawn).expect("handle");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        assert_eq!(
            pm.process_requested_cnode_slots(spawned.pid),
            Some(Some(96))
        );
        assert_eq!(pm.process_requested_task_class(spawned.pid), Some(None));
    }

    #[test]
    fn process_manager_v4_spawn_records_requested_slots_and_task_class() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V4,
            0,
            None,
            &SpawnV4Args::new(1, 2, 96, 1).encode(),
        )
        .expect("msg");
        let spawn_reply = pm.handle_request(spawn).expect("handle");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        assert_eq!(
            pm.process_requested_cnode_slots(spawned.pid),
            Some(Some(96))
        );
        assert_eq!(
            pm.process_requested_task_class(spawned.pid),
            Some(Some(TaskClass::Driver))
        );
    }

    #[test]
    fn elf_parser_rejects_non_elf64_class() {
        let mut image = ProcessManager::synthetic_elf_image(1);
        image[4] = 1;
        assert_eq!(
            ElfImageInfo::parse(1, &image),
            Err(ProcessManagerError::Unsupported)
        );
    }

    #[test]
    fn elf_parser_rejects_wrong_machine() {
        let mut image = ProcessManager::synthetic_elf_image(1);
        image[18..20].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(
            ElfImageInfo::parse(1, &image),
            Err(ProcessManagerError::Unsupported)
        );
    }

    #[test]
    fn elf_parser_rejects_missing_load_segments() {
        let mut image = ProcessManager::synthetic_elf_image(1);
        image[64..68].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            ElfImageInfo::parse(1, &image),
            Err(ProcessManagerError::Malformed)
        );
    }

    #[test]
    fn process_manager_spawn_allocates_pid_and_wait_observes_exit() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("msg");
        let spawn_reply = pm.handle_request(spawn).expect("handle");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        let info = pm.process_image_info(spawned.pid).expect("image info");
        assert_eq!(info.image_id, 2);
        assert_eq!(info.entry, 0x402000);

        pm.mark_exit(spawned.pid, 17).expect("mark exit");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        let wait_reply = pm.handle_request(wait).expect("wait reply");
        let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("decode");
        assert_eq!(waited.waited_pid, spawned.pid);
        assert_eq!(waited.exit_code, 17);
    }

    #[test]
    fn waitpid_returns_would_block_for_running_child() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 3).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::WouldBlock)
        );
    }

    #[test]
    fn exit_request_marks_caller_process_exited() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 5).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        let exit = Message::with_header(spawned.pid.0, PROC_OP_EXIT, 0, None, &9u64.to_le_bytes())
            .expect("exit");
        let _ = pm.handle_request(exit).expect("exit reply");
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        let wait_reply = pm.handle_request(wait).expect("wait reply");
        let waited = WaitPidV2Result::decode(wait_reply.as_slice()).expect("decode");
        assert_eq!(waited.exit_code, 9);
    }

    #[test]
    fn process_manager_rejects_cap_transport() {
        let msg = Message::with_header(
            0,
            PROC_OP_GETPID,
            Message::FLAG_CAP_TRANSFER,
            Some(9),
            &7u64.to_le_bytes(),
        )
        .expect("msg");
        assert_eq!(
            ProcessManager::parse_request(msg),
            Err(ProcessManagerError::InvalidTransport)
        );
    }

    #[test]
    fn waitpid_reaps_exited_child() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");
        pm.mark_exit(spawned.pid, 4).expect("exit");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, spawned.pid.0).encode(),
        )
        .expect("wait");
        let _ = pm.handle_request(wait).expect("wait reply");
        assert_eq!(pm.live_process_count(), 0);
    }

    #[test]
    fn waitpid_rejects_non_parent_caller() {
        let mut pm = ProcessManager::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let spawn_reply = pm.handle_request(spawn).expect("spawn reply");
        let spawned = SpawnV2Result::decode(spawn_reply.as_slice()).expect("decode");

        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(99, spawned.pid.0).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::PermissionDenied)
        );
    }

    #[test]
    fn waitpid_rejects_unknown_target() {
        let mut pm = ProcessManager::new();
        let wait = Message::with_header(
            0,
            PROC_OP_WAITPID_V2,
            0,
            None,
            &WaitPidV2Args::new(1, 4242).encode(),
        )
        .expect("wait");
        assert_eq!(
            pm.handle_request(wait),
            Err(ProcessManagerError::PermissionDenied)
        );
    }

    #[test]
    fn process_service_tracks_batch_handled() {
        let mut service = ProcessService::new();
        let spawn = Message::with_header(
            0,
            PROC_OP_SPAWN_V2,
            0,
            None,
            &SpawnV2Args::new(1, 2).encode(),
        )
        .expect("spawn");
        let getpid =
            Message::with_header(0, PROC_OP_GETPID, 0, None, &42u64.to_le_bytes()).expect("getpid");

        let handled = service.handle_batch([spawn, getpid]).expect("batch");
        assert_eq!(handled, 2);
        assert_eq!(service.handled_count(), 2);
    }

    #[test]
    fn process_manager_tracks_explicit_thread_identities() {
        let mut pm = ProcessManager::new();
        let pid = pm.alloc_process(ProcessId(1)).expect("pid");
        pm.register_thread_identity(pid, 2000, ThreadGroupId(pid.0))
            .expect("thread");
        assert_eq!(pm.process_id_for_tid(2000), pid);
        assert_eq!(
            pm.thread_identity(2000).expect("identity").thread_group_id,
            ThreadGroupId(pid.0)
        );
    }
}
