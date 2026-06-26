// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

//! Non-live supervisor↔PM restart contract/model home.
//!
//! This module is intentionally compiled only for hosted development and tests.
//! SUP-11 keeps production restart execution in `service.rs` fail-closed and
//! deferred; any future live PM restart client must route through the modeled
//! contract instead of direct helper bypasses.

#![cfg(any(test, feature = "hosted-dev"))]

/// Source guard marker documenting that restart contract/model code is non-live.
pub const SUPERVISOR_RESTART_MODEL_NON_LIVE: &str = "SUPERVISOR_RESTART_MODEL_NON_LIVE";
