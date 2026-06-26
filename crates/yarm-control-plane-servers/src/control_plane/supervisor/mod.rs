// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

pub mod service;

pub use service::{ManagedServiceKind, SupervisorDecision, SupervisorService, run};
