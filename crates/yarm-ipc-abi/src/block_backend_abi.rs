// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub const BLK_BACKEND_OP_QUERY_STATE: u16 = 1;
pub const BLK_BACKEND_OP_READ: u16 = 2;
pub const BLK_BACKEND_OP_WRITE: u16 = 3;
pub const BLK_BACKEND_OP_FLUSH: u16 = 4;
pub const BLK_BACKEND_OP_GET_GEOM: u16 = 5;

pub const BLK_BACKEND_STATUS_OK: i32 = 0;
pub const BLK_BACKEND_STATUS_EAGAIN: i32 = -11;
pub const BLK_BACKEND_STATUS_ENOSYS: i32 = -38;
pub const BLK_BACKEND_STATUS_ENODEV: i32 = -19;
pub const BLK_BACKEND_STATUS_EIO: i32 = -5;
pub const BLK_BACKEND_STATUS_EINVAL: i32 = -22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkSgEntry {
    pub mem_cap: u64,
    pub offset: u64,
    pub length: u32,
    pub flags: u32,
}
impl BlkSgEntry {
    pub const ENCODED_LEN: usize = 24;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0u8; Self::ENCODED_LEN];
        o[0..8].copy_from_slice(&self.mem_cap.to_le_bytes());
        o[8..16].copy_from_slice(&self.offset.to_le_bytes());
        o[16..20].copy_from_slice(&self.length.to_le_bytes());
        o[20..24].copy_from_slice(&self.flags.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN { return None; }
        Some(Self {
            mem_cap: u64::from_le_bytes(b[0..8].try_into().ok()?),
            offset: u64::from_le_bytes(b[8..16].try_into().ok()?),
            length: u32::from_le_bytes(b[16..20].try_into().ok()?),
            flags: u32::from_le_bytes(b[20..24].try_into().ok()?),
        })
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkBackendQueryRequest {
    pub req_id: u32,
    pub flags: u32,
    pub device_id: u64,
}
impl BlkBackendQueryRequest {
    pub const ENCODED_LEN: usize = 16;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o=[0u8; Self::ENCODED_LEN];
        o[0..4].copy_from_slice(&self.req_id.to_le_bytes());
        o[4..8].copy_from_slice(&self.flags.to_le_bytes());
        o[8..16].copy_from_slice(&self.device_id.to_le_bytes());
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN { return None; }
        Some(Self {
            req_id: u32::from_le_bytes(b[0..4].try_into().ok()?),
            flags: u32::from_le_bytes(b[4..8].try_into().ok()?),
            device_id: u64::from_le_bytes(b[8..16].try_into().ok()?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkBackendRequest {
    pub req_id: u32,
    pub flags: u32,
    pub device_id: u64,
    pub sector_start: u64,
    pub sector_count: u32,
    pub sg_count: u32,
    pub sg_list: [BlkSgEntry; 4],
}
impl BlkBackendRequest {
    pub const ENCODED_LEN: usize = 128;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o = [0u8; Self::ENCODED_LEN];
        o[0..4].copy_from_slice(&self.req_id.to_le_bytes());
        o[4..8].copy_from_slice(&self.flags.to_le_bytes());
        o[8..16].copy_from_slice(&self.device_id.to_le_bytes());
        o[16..24].copy_from_slice(&self.sector_start.to_le_bytes());
        o[24..28].copy_from_slice(&self.sector_count.to_le_bytes());
        o[28..32].copy_from_slice(&self.sg_count.to_le_bytes());
        for (i, sg) in self.sg_list.iter().enumerate() {
            let start = 32 + i * BlkSgEntry::ENCODED_LEN;
            let end = start + BlkSgEntry::ENCODED_LEN;
            o[start..end].copy_from_slice(&sg.encode());
        }
        o
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED_LEN { return None; }
        let mut sg_list = [BlkSgEntry { mem_cap:0, offset:0, length:0, flags:0 }; 4];
        for (i, sg) in sg_list.iter_mut().enumerate() {
            let start = 32 + i * BlkSgEntry::ENCODED_LEN;
            let end = start + BlkSgEntry::ENCODED_LEN;
            *sg = BlkSgEntry::decode(&b[start..end])?;
        }
        let out = Self {
            req_id: u32::from_le_bytes(b[0..4].try_into().ok()?),
            flags: u32::from_le_bytes(b[4..8].try_into().ok()?),
            device_id: u64::from_le_bytes(b[8..16].try_into().ok()?),
            sector_start: u64::from_le_bytes(b[16..24].try_into().ok()?),
            sector_count: u32::from_le_bytes(b[24..28].try_into().ok()?),
            sg_count: u32::from_le_bytes(b[28..32].try_into().ok()?),
            sg_list,
        };
        if out.sg_count > 4 { return None; }
        Some(out)
    }
    pub fn is_valid_for_opcode(&self, opcode: u16) -> bool {
        match opcode {
            BLK_BACKEND_OP_READ | BLK_BACKEND_OP_WRITE => self.sg_count > 0 && self.sg_count <= 4,
            BLK_BACKEND_OP_QUERY_STATE | BLK_BACKEND_OP_GET_GEOM | BLK_BACKEND_OP_FLUSH => self.sg_count <= 4,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlkBackendResponse {
    pub req_id: u32,
    pub status: i32,
    pub actual_bytes: u64,
    pub backend_generation: u64,
    pub logical_block_size: u32,
    pub physical_block_size: u32,
}
impl BlkBackendResponse {
    pub const ENCODED_LEN: usize = 32;
    pub fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let mut o=[0u8; Self::ENCODED_LEN];
        o[0..4].copy_from_slice(&self.req_id.to_le_bytes());
        o[4..8].copy_from_slice(&self.status.to_le_bytes());
        o[8..16].copy_from_slice(&self.actual_bytes.to_le_bytes());
        o[16..24].copy_from_slice(&self.backend_generation.to_le_bytes());
        o[24..28].copy_from_slice(&self.logical_block_size.to_le_bytes());
        o[28..32].copy_from_slice(&self.physical_block_size.to_le_bytes());
        o
    }
    pub fn decode(b:&[u8])->Option<Self>{
        if b.len()!=Self::ENCODED_LEN { return None; }
        Some(Self{
            req_id:u32::from_le_bytes(b[0..4].try_into().ok()?),
            status:i32::from_le_bytes(b[4..8].try_into().ok()?),
            actual_bytes:u64::from_le_bytes(b[8..16].try_into().ok()?),
            backend_generation:u64::from_le_bytes(b[16..24].try_into().ok()?),
            logical_block_size:u32::from_le_bytes(b[24..28].try_into().ok()?),
            physical_block_size:u32::from_le_bytes(b[28..32].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn frozen_opcodes(){assert_eq!(BLK_BACKEND_OP_QUERY_STATE,1);assert_eq!(BLK_BACKEND_OP_GET_GEOM,5);}    
    #[test] fn frozen_status(){assert_eq!(BLK_BACKEND_STATUS_EAGAIN,-11);assert_eq!(BLK_BACKEND_STATUS_ENOSYS,-38);}    
    #[test] fn sg_roundtrip(){let e=BlkSgEntry{mem_cap:1,offset:2,length:3,flags:4};assert_eq!(BlkSgEntry::decode(&e.encode()),Some(e));}
    #[test] fn query_req_roundtrip_and_small(){let q=BlkBackendQueryRequest{req_id:1,flags:2,device_id:3};assert_eq!(BlkBackendQueryRequest::decode(&q.encode()),Some(q));assert!(BlkBackendQueryRequest::ENCODED_LEN < BlkBackendRequest::ENCODED_LEN);}
    #[test] fn req_roundtrip(){let r=BlkBackendRequest{req_id:1,flags:2,device_id:3,sector_start:4,sector_count:5,sg_count:1,sg_list:[BlkSgEntry{mem_cap:6,offset:7,length:8,flags:9};4]};assert_eq!(BlkBackendRequest::decode(&r.encode()),Some(r));}
    #[test] fn resp_roundtrip(){let r=BlkBackendResponse{req_id:1,status:BLK_BACKEND_STATUS_EAGAIN,actual_bytes:0,backend_generation:0,logical_block_size:512,physical_block_size:512};assert_eq!(BlkBackendResponse::decode(&r.encode()),Some(r));}
    #[test] fn reject_sg_count_gt4(){let mut b=[0u8;BlkBackendRequest::ENCODED_LEN];b[28..32].copy_from_slice(&5u32.to_le_bytes());assert!(BlkBackendRequest::decode(&b).is_none());}
    #[test] fn short_rejected(){assert!(BlkSgEntry::decode(&[0;23]).is_none());assert!(BlkBackendQueryRequest::decode(&[0;15]).is_none());assert!(BlkBackendRequest::decode(&[0;127]).is_none());assert!(BlkBackendResponse::decode(&[0;31]).is_none());}
    #[test] fn read_write_require_sg(){let r=BlkBackendRequest{req_id:1,flags:0,device_id:0,sector_start:0,sector_count:1,sg_count:0,sg_list:[BlkSgEntry{mem_cap:0,offset:0,length:0,flags:0};4]};assert!(!r.is_valid_for_opcode(BLK_BACKEND_OP_READ));assert!(!r.is_valid_for_opcode(BLK_BACKEND_OP_WRITE));}
    #[test] fn query_allows_zero_sg(){let r=BlkBackendRequest{req_id:1,flags:0,device_id:0,sector_start:0,sector_count:0,sg_count:0,sg_list:[BlkSgEntry{mem_cap:0,offset:0,length:0,flags:0};4]};assert!(r.is_valid_for_opcode(BLK_BACKEND_OP_QUERY_STATE));}

    #[test] fn response_golden_vector(){
        let r=BlkBackendResponse{req_id:1,status:BLK_BACKEND_STATUS_EAGAIN,actual_bytes:0,backend_generation:0,logical_block_size:512,physical_block_size:512};
        let b=r.encode();
        let mut exp=[0u8;BlkBackendResponse::ENCODED_LEN];
        exp[0..4].copy_from_slice(&1u32.to_le_bytes());
        exp[4..8].copy_from_slice(&(-11i32).to_le_bytes());
        exp[24..28].copy_from_slice(&512u32.to_le_bytes());
        exp[28..32].copy_from_slice(&512u32.to_le_bytes());
        assert_eq!(b,exp);
        assert_eq!(BlkBackendResponse::decode(&exp),Some(r));
    }
}
