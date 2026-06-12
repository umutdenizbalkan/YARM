// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Virtio block queue, frame, and in-memory device implementation.
//!
//! The in-memory device preserves the existing deterministic service behavior;
//! it is not a claim of production virtio hardware support.

pub mod device;

pub use device::{
    VIRTIO_BLK_OP_READ, VIRTIO_BLK_OP_WRITE, VirtQueue, VirtioBlkDevice, VirtioBlkMemoryDevice,
    VirtioBlkReqFrame, VirtioBlkRequest, VirtioBlkRespFrame, VirtqChain, VirtqDescRole,
    VirtqDescriptor, build_write_chain,
};
