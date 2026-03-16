#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkRequest {
    pub sector: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlkDevice {
    pub sectors: u64,
    pub sector_size: u64,
    pub reads: u64,
    pub writes: u64,
}

impl Default for VirtioBlkDevice {
    fn default() -> Self {
        Self::new(4096, 512)
    }
}

impl VirtioBlkDevice {
    pub const fn new(sectors: u64, sector_size: u64) -> Self {
        Self {
            sectors,
            sector_size,
            reads: 0,
            writes: 0,
        }
    }

    pub fn read(&mut self, req: VirtioBlkRequest) -> Result<u64, ()> {
        if req.sector >= self.sectors {
            return Err(());
        }
        self.reads = self.reads.saturating_add(1);
        Ok(req.len)
    }

    pub fn write(&mut self, req: VirtioBlkRequest) -> Result<u64, ()> {
        if req.sector >= self.sectors {
            return Err(());
        }
        self.writes = self.writes.saturating_add(1);
        Ok(req.len)
    }
}
