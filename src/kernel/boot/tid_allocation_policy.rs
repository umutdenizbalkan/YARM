// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TidAllocationPolicy {
    static_tid_upper_bound: u64,
    dynamic_tid_floor: u64,
}

impl TidAllocationPolicy {
    pub(crate) const fn new(static_tid_upper_bound: u64, dynamic_tid_floor: u64) -> Self {
        Self {
            static_tid_upper_bound,
            dynamic_tid_floor,
        }
    }

    pub(crate) const fn static_tid_upper_bound(self) -> u64 {
        self.static_tid_upper_bound
    }

    pub(crate) const fn dynamic_tid_floor(self) -> u64 {
        self.dynamic_tid_floor
    }

    pub(crate) const fn normalize_dynamic_cursor(self, cursor: u64) -> u64 {
        if cursor < self.dynamic_tid_floor {
            self.dynamic_tid_floor
        } else {
            cursor
        }
    }

    pub(crate) const fn advance_dynamic_cursor(self, cursor: u64) -> u64 {
        let next = cursor.wrapping_add(1);
        self.normalize_dynamic_cursor(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TidAllocationCursor {
    next_dynamic_tid: u64,
}

impl TidAllocationCursor {
    pub(crate) const fn new(policy: TidAllocationPolicy) -> Self {
        Self {
            next_dynamic_tid: policy.dynamic_tid_floor(),
        }
    }

    pub(crate) const fn next_dynamic_tid(self, policy: TidAllocationPolicy) -> u64 {
        policy.normalize_dynamic_cursor(self.next_dynamic_tid)
    }

    pub(crate) const fn raw_next_dynamic_tid(self) -> u64 {
        self.next_dynamic_tid
    }

    pub(crate) fn advance_after_allocation(
        &mut self,
        policy: TidAllocationPolicy,
        allocated_tid: u64,
    ) {
        self.next_dynamic_tid = policy.advance_dynamic_cursor(allocated_tid);
    }

    #[cfg(test)]
    pub(crate) fn set_next_dynamic_tid_for_test(&mut self, next_dynamic_tid: u64) {
        self.next_dynamic_tid = next_dynamic_tid;
    }
}

#[cfg(test)]
mod tests {
    use super::{TidAllocationCursor, TidAllocationPolicy};

    #[test]
    fn cursor_normalizes_to_dynamic_floor() {
        let policy = TidAllocationPolicy::new(9_999, 10_000);
        let mut cursor = TidAllocationCursor::new(policy);
        cursor.set_next_dynamic_tid_for_test(42);
        assert_eq!(cursor.next_dynamic_tid(policy), 10_000);
    }

    #[test]
    fn cursor_wraps_to_dynamic_floor() {
        let policy = TidAllocationPolicy::new(9_999, 10_000);
        let mut cursor = TidAllocationCursor::new(policy);
        cursor.set_next_dynamic_tid_for_test(u64::MAX);
        let allocated = cursor.next_dynamic_tid(policy);
        assert_eq!(allocated, u64::MAX);
        cursor.advance_after_allocation(policy, allocated);
        assert_eq!(cursor.next_dynamic_tid(policy), 10_000);
    }
}
