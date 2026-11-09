
    /// Lock order, structurally: no Notification reference is reserved or released while a
    /// capability (rank 4) critical section is open. Every `with_capability_state*` closure in
    /// the owners' files is matched to its closing parenthesis, and none may contain a reserve or
    /// release call — rank 3 is only ever entered after rank 4 has been released.
    #[test]
    fn no_notification_reference_moves_inside_a_capability_critical_section() {
        fn closures<'a>(src: &'a str, opener: &str) -> alloc::vec::Vec<&'a str> {
            let mut out = alloc::vec::Vec::new();
            let mut from = 0usize;
            while let Some(rel) = src[from..].find(opener) {
                let start = from + rel + opener.len();
                let bytes = src.as_bytes();
                let mut depth = 1usize;
                let mut i = start;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                out.push(&src[start..i]);
                from = i;
            }
            out
        }
        let files: &[(&str, &str)] = &[
            ("capability_lifecycle_state.rs", include_str!("capability_lifecycle_state.rs")),
            ("capability_state.rs", include_str!("capability_state.rs")),
            ("cap_memory_mint_split.rs", include_str!("cap_memory_mint_split.rs")),
            ("cnode_state.rs", include_str!("cnode_state.rs")),
            ("ipc_state.rs", include_str!("ipc_state.rs")),
            ("notification_lifetime.rs", include_str!("notification_lifetime.rs")),
            ("provisional_cap.rs", include_str!("provisional_cap.rs")),
            ("spawn_txn.rs", include_str!("../syscall/spawn_txn.rs")),
            ("reap_txn.rs", include_str!("../syscall/reap_txn.rs")),
            ("runtime.rs", include_str!("../../runtime.rs")),
        ];
        let mut scanned = 0usize;
        for (file, src) in files {
            let code = src
                .find("\n#[cfg(test)]\nmod tests {")
                .map_or(*src, |cut| &src[..cut]);
            for opener in [
                "with_capability_state_mut(",
                "with_capability_state(",
                "with_capability_state_split_mut(",
            ] {
                for body in closures(code, opener) {
                    scanned += 1;
                    for token in [
                        "reserve_notification_ref",
                        "release_notification_ref",
                        "with_ipc_state",
                        "with_ipc_split_mut",
                    ] {
                        assert!(
                            !body.contains(token),
                            "{file}: `{token}` inside a `{opener}` closure — rank 3 under rank 4"
                        );
                    }
                }
            }
        }
        assert!(scanned > 50, "the scan must actually reach the owners' closures ({scanned})");
    }
