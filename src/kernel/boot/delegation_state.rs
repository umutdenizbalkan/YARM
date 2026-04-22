// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

use super::*;
use alloc::boxed::Box;
use alloc::vec::Vec;

impl KernelState {
    fn contains_cap_ref(set: &[DelegatedCapRef], needle: DelegatedCapRef) -> bool {
        set.contains(&needle)
    }

    pub(crate) fn collect_delegated_descendants(
        &self,
        root: DelegatedCapRef,
    ) -> Vec<DelegatedCapRef> {
        let links_snapshot = Box::new(
            self.with_capability_state(|capability| capability.delegated_capability_links.clone()),
        );
        let mut found = Vec::with_capacity(64);
        let mut queue = Vec::with_capacity(64);
        let mut head = 0usize;
        queue.push(root);
        while head < queue.len() {
            let current = queue[head];
            head += 1;
            for link in links_snapshot.iter().flatten() {
                let link_source_pid = self.process_id(link.source_tid).unwrap_or(link.source_tid);
                if link_source_pid != current.pid || link.source_cap != current.cap {
                    continue;
                }
                let child = DelegatedCapRef {
                    pid: self.process_id(link.dest_tid).unwrap_or(link.dest_tid),
                    cap: link.dest_cap,
                };
                if Self::contains_cap_ref(&found, child) {
                    continue;
                }
                if found.len() >= MAX_DELEGATED_CAPABILITY_LINKS
                    || queue.len() >= MAX_DELEGATED_CAPABILITY_LINKS
                {
                    break;
                }
                found.push(child);
                queue.push(child);
            }
        }
        found
    }

    pub(crate) fn remove_delegation_links_for(
        &mut self,
        root: DelegatedCapRef,
        descendants: &[DelegatedCapRef],
    ) {
        let link_snapshot = Box::new(
            self.with_capability_state(|capability| capability.delegated_capability_links.clone()),
        );
        let mut remove_links = Box::new([false; MAX_DELEGATED_CAPABILITY_LINKS]);
        for (idx, maybe_link) in link_snapshot.iter().enumerate() {
            let Some(link) = maybe_link else {
                continue;
            };
            let source = DelegatedCapRef {
                pid: self.process_id(link.source_tid).unwrap_or(link.source_tid),
                cap: link.source_cap,
            };
            let dest = DelegatedCapRef {
                pid: self.process_id(link.dest_tid).unwrap_or(link.dest_tid),
                cap: link.dest_cap,
            };
            let involved = source == root
                || dest == root
                || Self::contains_cap_ref(descendants, source)
                || Self::contains_cap_ref(descendants, dest);
            if involved {
                remove_links[idx] = true;
            }
        }
        self.with_capability_state_mut(|capability| {
            for (idx, remove) in remove_links.iter().enumerate() {
                if *remove {
                    capability.delegated_capability_links[idx] = None;
                }
            }
        });
    }

    pub(crate) fn capability_object_live(&self, object: CapObject) -> Option<()> {
        match object {
            CapObject::Endpoint { index, generation } => {
                if index >= MAX_ENDPOINTS
                    || self.with_ipc_state(|ipc| ipc.endpoint_generations[index]) != generation
                {
                    return None;
                }
            }
            CapObject::Notification { index, generation } => {
                if index >= MAX_NOTIFICATIONS
                    || self.with_ipc_state(|ipc| ipc.notification_generations[index]) != generation
                {
                    return None;
                }
            }
            CapObject::Reply { index, generation } => {
                if index >= MAX_REPLY_CAPS
                    || self.with_ipc_state(|ipc| ipc.reply_cap_generations[index]) != generation
                {
                    return None;
                }
            }
            _ => {}
        }
        Some(())
    }
}
