// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Kernel-facing trap types are normalized in `crate::arch::trap`.
//! This module re-exports the normalized surface to preserve existing
//! kernel/service call sites during the PR-B trap-normalization split.

pub use crate::arch::trap::{
    FaultAccess, FaultInfo, IrqNumber, Trap, TrapAction, TrapEvent, route_trap,
};
