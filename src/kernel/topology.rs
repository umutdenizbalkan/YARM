use crate::arch::platform_layout;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

pub const MAX_CPUS: usize = platform_layout::MAX_CPUS;
pub type CpuBitmap = u64;
const _: () = assert!(
    MAX_CPUS <= CpuBitmap::BITS as usize,
    "MAX_CPUS exceeds CpuBitmap width; increase CpuBitmap",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyError {
    CpuNotPresent,
    CpuAlreadyOnline,
    CpuNotStarted,
    AckNotReceived,
    InvalidCpuId,
}

impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::CpuNotPresent => "CPU is not present in the topology",
            Self::CpuAlreadyOnline => "CPU is already online",
            Self::CpuNotStarted => "CPU has not been started",
            Self::AckNotReceived => "secondary CPU has not acknowledged bring-up",
            Self::InvalidCpuId => "CPU identifier is outside the topology bitmap",
        };
        f.write_str(message)
    }
}

#[derive(Debug)]
pub struct CpuTopology {
    present: AtomicU64,
    online: AtomicU64,
    started: AtomicU64,
    pending_ack: AtomicU64,
}

impl CpuTopology {
    pub const fn new(present: CpuBitmap, online: CpuBitmap) -> Self {
        Self {
            present: AtomicU64::new(present),
            online: AtomicU64::new(online),
            started: AtomicU64::new(0),
            pending_ack: AtomicU64::new(0),
        }
    }

    pub const fn bootstrap_only() -> Self {
        Self::new(1, 1)
    }

    pub fn from_present_bitmap(present: CpuBitmap) -> Self {
        let bootstrap = 1u64 << platform_layout::BOOTSTRAP_CPU_ID;
        let masked = present & Self::valid_mask();
        debug_assert!(
            (masked & bootstrap) != 0,
            "present bitmap must explicitly include the bootstrap CPU"
        );
        let present = if masked == 0 {
            bootstrap
        } else {
            masked | bootstrap
        };
        Self::new(present, bootstrap)
    }

    pub const fn valid_mask() -> CpuBitmap {
        if MAX_CPUS >= CpuBitmap::BITS as usize {
            CpuBitmap::MAX
        } else {
            (1u64 << MAX_CPUS) - 1
        }
    }

    pub fn present_cpu_bitmap(&self) -> CpuBitmap {
        self.present.load(Ordering::Acquire)
    }

    pub fn online_cpu_bitmap(&self) -> CpuBitmap {
        self.online.load(Ordering::Acquire)
    }

    pub fn present_cpu_count(&self) -> usize {
        self.present_cpu_bitmap().count_ones() as usize
    }

    pub fn online_cpu_count(&self) -> usize {
        self.online_cpu_bitmap().count_ones() as usize
    }

    pub fn cpu_present(&self, cpu: u8) -> bool {
        let mask = 1u64.checked_shl(cpu as u32).unwrap_or(0);
        (self.present_cpu_bitmap() & mask) != 0
    }

    pub fn cpu_online(&self, cpu: u8) -> bool {
        let mask = 1u64.checked_shl(cpu as u32).unwrap_or(0);
        (self.online_cpu_bitmap() & mask) != 0
    }

    pub fn detect_secondary_cpus(&self) -> [Option<u8>; MAX_CPUS] {
        let mut out = [None; MAX_CPUS];
        let mut count = 0usize;
        for cpu in 0..MAX_CPUS {
            let cpu_id = cpu as u8;
            if cpu_id != platform_layout::BOOTSTRAP_CPU_ID && self.cpu_present(cpu_id) {
                out[count] = Some(cpu_id);
                count += 1;
            }
        }
        out
    }

    pub fn start_secondary_cpu(&self, cpu: u8) -> Result<(), TopologyError> {
        let mask = 1u64
            .checked_shl(cpu as u32)
            .ok_or(TopologyError::InvalidCpuId)?;
        if !self.cpu_present(cpu) {
            return Err(TopologyError::CpuNotPresent);
        }
        if self.cpu_online(cpu) {
            return Err(TopologyError::CpuAlreadyOnline);
        }
        self.started.fetch_or(mask, Ordering::AcqRel);
        self.pending_ack.fetch_or(mask, Ordering::AcqRel);
        Ok(())
    }

    pub fn acknowledge_secondary_cpu(&self, cpu: u8) -> Result<(), TopologyError> {
        let mask = 1u64
            .checked_shl(cpu as u32)
            .ok_or(TopologyError::InvalidCpuId)?;
        if (self.started.load(Ordering::Acquire) & mask) == 0 {
            return Err(TopologyError::CpuNotStarted);
        }
        if (self.pending_ack.load(Ordering::Acquire) & mask) == 0 {
            return Err(TopologyError::AckNotReceived);
        }
        self.pending_ack.fetch_and(!mask, Ordering::AcqRel);
        Ok(())
    }

    pub fn check_secondary_ack(&self, cpu: u8) -> Result<(), TopologyError> {
        let mask = 1u64
            .checked_shl(cpu as u32)
            .ok_or(TopologyError::InvalidCpuId)?;
        if (self.started.load(Ordering::Acquire) & mask) == 0 {
            return Err(TopologyError::CpuNotStarted);
        }
        if (self.pending_ack.load(Ordering::Acquire) & mask) != 0 {
            return Err(TopologyError::AckNotReceived);
        }
        Ok(())
    }

    pub fn mark_cpu_offline(&self, cpu: u8) -> Result<(), TopologyError> {
        let mask = 1u64
            .checked_shl(cpu as u32)
            .ok_or(TopologyError::InvalidCpuId)?;
        if !self.cpu_online(cpu) {
            return Err(TopologyError::CpuNotPresent);
        }
        self.online.fetch_and(!mask, Ordering::AcqRel);
        self.started.fetch_and(!mask, Ordering::AcqRel);
        self.pending_ack.fetch_and(!mask, Ordering::AcqRel);
        Ok(())
    }

    pub fn mark_cpu_online(&self, cpu: u8) -> Result<(), TopologyError> {
        let mask = 1u64
            .checked_shl(cpu as u32)
            .ok_or(TopologyError::InvalidCpuId)?;
        if !self.cpu_present(cpu) {
            return Err(TopologyError::CpuNotPresent);
        }
        if self.cpu_online(cpu) {
            return Err(TopologyError::CpuAlreadyOnline);
        }
        self.check_secondary_ack(cpu)?;
        self.online.fetch_or(mask, Ordering::AcqRel);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_tracks_present_separately_from_online() {
        let topo = CpuTopology::from_present_bitmap(0b111);
        assert_eq!(topo.present_cpu_count(), 3);
        assert_eq!(topo.present_cpu_bitmap(), 0b111);
        assert_eq!(topo.online_cpu_count(), 1);
        assert_eq!(topo.online_cpu_bitmap(), 0b001);
    }

    #[test]
    fn secondary_bring_up_requires_ack_before_online() {
        let topo = CpuTopology::from_present_bitmap(0b11);
        assert!(topo.start_secondary_cpu(1).is_ok());
        assert!(topo.mark_cpu_online(1).is_err());
        topo.acknowledge_secondary_cpu(1).expect("ack");
        topo.check_secondary_ack(1).expect("wait");
        topo.mark_cpu_online(1).expect("online");
        assert!(topo.cpu_online(1));
    }

    #[test]
    fn mark_cpu_offline_clears_online_started_and_pending_ack() {
        let topo = CpuTopology::from_present_bitmap(0b11);
        topo.start_secondary_cpu(1).expect("start");
        topo.acknowledge_secondary_cpu(1).expect("ack");
        topo.mark_cpu_online(1).expect("online");

        topo.mark_cpu_offline(1).expect("offline");
        assert!(!topo.cpu_online(1));
        assert_eq!(
            topo.check_secondary_ack(1),
            Err(TopologyError::CpuNotStarted)
        );
        assert_eq!(topo.mark_cpu_offline(1), Err(TopologyError::CpuNotPresent));
    }
}
