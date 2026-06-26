// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[cfg(any(test, feature = "hosted-dev"))]
pub mod restart_model;
pub mod service;

pub use service::{ManagedServiceKind, SupervisorDecision, SupervisorService, run};
