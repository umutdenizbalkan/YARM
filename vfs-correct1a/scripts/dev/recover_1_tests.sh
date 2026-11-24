cd /tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad/vc1a-wt/crates/yarm-user-rt/src; python3 - <<'EOF'
p="vfs_client.rs"
s=open(p).read()
anchor='''    // ── Phase 2B build_bulk_read_message tests ────────────────────────────────'''
assert s.count(anchor)==1
tests='''    // ── VFS-CORRECT1A: the transaction through a scripted transport ─────────────

    use yarm_ipc_abi::vfs_abi::{
        VFS_OP_FILE_GRANT_RO, VFS_STATUS_ERR_BAD_FD, VFS_STATUS_ERR_INVALID_PATH, VfsCompletion,
    };

    const SENTINEL: u8 = 0xA5;

    /// One scripted reply: payload, transferred cap, reply cap.
    #[derive(Clone, Copy)]
    struct Scripted {
        payload: [u8; 128],
        len: usize,
        transferred_cap: Option<u32>,
        reply_cap: Option<u32>,
    }

    impl Scripted {
        fn completion(c: VfsCompletion, data: &[u8]) -> Self {
            let (payload, len) = c.encode(data).expect("encode");
            Self {
                payload,
                len,
                transferred_cap: None,
                reply_cap: None,
            }
        }
        fn raw(bytes: &[u8]) -> Self {
            let mut payload = [0u8; 128];
            payload[..bytes.len()].copy_from_slice(bytes);
            Self {
                payload,
                len: bytes.len(),
                transferred_cap: None,
                reply_cap: None,
            }
        }
    }

    /// Records every call and receive; replies from a fixed script.
    struct ScriptTransport {
        send_result: Result<(), SyscallError>,
        replies: [Option<Scripted>; 4],
        next: usize,
        calls: usize,
        recvs: usize,
        last_request: Option<Message>,
    }

    impl ScriptTransport {
        fn new(send_result: Result<(), SyscallError>) -> Self {
            Self {
                send_result,
                replies: [None; 4],
                next: 0,
                calls: 0,
                recvs: 0,
                last_request: None,
            }
        }
        fn reply(mut self, r: Scripted) -> Self {
            let i = self.replies.iter().position(|x| x.is_none()).expect("room");
            self.replies[i] = Some(r);
            self
        }
    }

    impl VfsTransport for ScriptTransport {
        fn call(&mut self, msg: &Message) -> Result<(), SyscallError> {
            self.calls += 1;
            self.last_request = Some(*msg);
            self.send_result
        }
        fn recv(&mut self) -> Result<Option<ReceivedMessage>, SyscallError> {
            self.recvs += 1;
            let Some(r) = self.replies.get(self.next).copied().flatten() else {
                return Ok(None);
            };
            self.next += 1;
            let flags = if r.transferred_cap.is_some() {
                Message::FLAG_CAP_TRANSFER
            } else {
                0
            };
            let message = Message::with_header(
                10000,
                0,
                flags,
                r.transferred_cap.map(u64::from),
                &r.payload[..r.len],
            )
            .expect("message");
            Ok(Some(ReceivedMessage {
                message,
                reply_cap: r.reply_cap,
                transferred_cap: r.transferred_cap,
                sender_tid: 10000,
            }))
        }
    }

    fn read_ok(fd: u64, data: &[u8]) -> Scripted {
        Scripted::completion(VfsCompletion::ok(VFS_OP_READ, data.len() as u64, fd), data)
    }

    #[test]
    fn vc1a_send_failure_makes_zero_receive_attempts() {
        for err in [
            SyscallError::WouldBlock,
            SyscallError::InvalidCapability,
            SyscallError::QueueFull,
        ] {
            let mut t = ScriptTransport::new(Err(err)).reply(read_ok(3, b"never"));
            let mut buf = [SENTINEL; 16];
            assert_eq!(vfs_read_with(&mut t, 3, &mut buf), Err(VfsClientError::Send(err)));
            assert_eq!((t.calls, t.recvs), (1, 0));
            assert!(buf.iter().all(|&b| b == SENTINEL));
            let mut t = ScriptTransport::new(Err(err));
            assert_eq!(
                vfs_openat_with(&mut t, b"/initramfs/x", 0),
                Err(VfsClientError::Send(err))
            );
            assert_eq!((t.calls, t.recvs), (1, 0));
            let mut t = ScriptTransport::new(Err(err));
            assert_eq!(vfs_statx_with(&mut t, b"/x"), Err(VfsClientError::Send(err)));
            assert_eq!((t.calls, t.recvs), (1, 0));
            let mut t = ScriptTransport::new(Err(err));
            assert_eq!(vfs_close_with(&mut t, 3), Err(VfsClientError::Send(err)));
            assert_eq!((t.calls, t.recvs), (1, 0));
        }
    }

    #[test]
    fn vc1a_read_copies_exactly_the_count_and_leaves_the_tail() {
        let mut t = ScriptTransport::new(Ok(())).reply(read_ok(7, b"hello"));
        let mut buf = [SENTINEL; 32];
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(5));
        assert_eq!(&buf[..5], b"hello");
        assert!(buf[5..].iter().all(|&b| b == SENTINEL));
        assert_eq!((t.calls, t.recvs), (1, 1));
        // The request asked for the slice length (32 < inline max).
        let req = t.last_request.expect("request");
        assert_eq!(ReadWriteArgs::decode(req.as_slice()).expect("args").len, 32);
    }

    #[test]
    fn vc1a_large_destination_asks_for_one_inline_reply_and_accepts_a_short_read() {
        let data = [0x3Cu8; VFS_INLINE_READ_MAX];
        let mut t = ScriptTransport::new(Ok(())).reply(read_ok(7, &data));
        let mut buf = [SENTINEL; 512];
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(VFS_INLINE_READ_MAX));
        assert!(buf[..VFS_INLINE_READ_MAX].iter().all(|&b| b == 0x3C));
        assert!(buf[VFS_INLINE_READ_MAX..].iter().all(|&b| b == SENTINEL));
        let req = t.last_request.expect("request");
        assert_eq!(
            ReadWriteArgs::decode(req.as_slice()).expect("args").len,
            VFS_INLINE_READ_MAX as u64
        );
    }

    #[test]
    fn vc1a_eof_repeats_and_zero_length_reads_are_valid() {
        let mut t = ScriptTransport::new(Ok(()))
            .reply(read_ok(7, &[]))
            .reply(read_ok(7, &[]));
        let mut buf = [SENTINEL; 8];
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(0));
        assert_eq!(vfs_read_with(&mut t, 7, &mut buf), Ok(0));
        assert!(buf.iter().all(|&b| b == SENTINEL));
        let mut t = ScriptTransport::new(Ok(())).reply(read_ok(7, &[]));
        let mut empty: [u8; 0] = [];
        assert_eq!(vfs_read_with(&mut t, 7, &mut empty), Ok(0));
        let req = t.last_request.expect("request");
        assert_eq!(ReadWriteArgs::decode(req.as_slice()).expect("args").len, 0);
    }

    #[test]
    fn vc1a_malformed_completions_leave_the_destination_untouched() {
        let mut over = VfsCompletion::ok(VFS_OP_READ, 3, 7);
        let cases: [(Scripted, VfsCompletionFault); 7] = [
            // Truncated: the legacy 8-byte ReadLen reply.
            (Scripted::raw(&5u64.to_le_bytes()), VfsCompletionFault::Truncated),
            // Count without bytes (the baseline defect's shape).
            (
                Scripted::completion(VfsCompletion::ok(VFS_OP_READ, 4, 7), &[]),
                VfsCompletionFault::CountMismatch {
                    count: 4,
                    data_len: 0,
                },
            ),
            // More bytes than the count.
            (
                {
                    over.value = 2;
                    Scripted::completion(over, b"abc")
                },
                VfsCompletionFault::CountMismatch {
                    count: 2,
                    data_len: 3,
                },
            ),
            // More than requested.
            (
                read_ok(7, &[1u8; 9]),
                VfsCompletionFault::CountExceedsRequest {
                    count: 9,
                    requested: 8,
                },
            ),
            // Another operation's completion.
            (
                Scripted::completion(VfsCompletion::ok(VFS_OP_CLOSE, 0, 7), &[]),
                VfsCompletionFault::WrongOp {
                    expected: VFS_OP_READ,
                    got: VFS_OP_CLOSE,
                },
            ),
            // Another fd's completion.
            (
                read_ok(8, b"z"),
                VfsCompletionFault::WrongFd {
                    expected: 7,
                    got: 8,
                },
            ),
            // A capability on a READ completion.
            (
                {
                    let mut r = read_ok(7, b"z");
                    r.transferred_cap = Some(77);
                    r
                },
                VfsCompletionFault::UnexpectedCap,
            ),
        ];
        for (reply, fault) in cases {
            let mut t = ScriptTransport::new(Ok(())).reply(reply);
            let mut buf = [SENTINEL; 8];
            assert_eq!(
                vfs_read_with(&mut t, 7, &mut buf),
                Err(VfsClientError::Malformed(fault))
            );
            assert!(buf.iter().all(|&b| b == SENTINEL), "{:?}", fault);
            // No retry: one call, one receive.
            assert_eq!((t.calls, t.recvs), (1, 1));
        }
        // A reply capability never belongs on a completion.
        let mut r = read_ok(7, b"z");
        r.reply_cap = Some(5);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        let mut buf = [SENTINEL; 8];
        assert_eq!(
            vfs_read_with(&mut t, 7, &mut buf),
            Err(VfsClientError::Malformed(VfsCompletionFault::UnexpectedCap))
        );
        assert!(buf.iter().all(|&b| b == SENTINEL));
    }

    #[test]
    fn vc1a_status_errors_are_distinct_from_transport_and_malformed() {
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::err(VFS_OP_READ, VFS_STATUS_ERR_BAD_FD, 99),
            &[],
        ));
        let mut buf = [SENTINEL; 8];
        assert_eq!(
            vfs_read_with(&mut t, 99, &mut buf),
            Err(VfsClientError::Status(VFS_STATUS_ERR_BAD_FD))
        );
        assert!(buf.iter().all(|&b| b == SENTINEL));
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::err(VFS_OP_OPENAT, VFS_STATUS_ERR_INVALID_PATH, 0),
            &[],
        ));
        assert_eq!(
            vfs_openat_with(&mut t, b"/initramfs/missing", 0),
            Err(VfsClientError::Status(VFS_STATUS_ERR_INVALID_PATH))
        );
        // Nothing received at all.
        let mut t = ScriptTransport::new(Ok(()));
        assert_eq!(vfs_close_with(&mut t, 3), Err(VfsClientError::NoReply));
        assert_eq!((t.calls, t.recvs), (1, 1));
    }

    #[test]
    fn vc1a_openat_statx_close_return_validated_values() {
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::ok(VFS_OP_OPENAT, 14, 0),
            &[],
        ));
        assert_eq!(vfs_openat_with(&mut t, b"/initramfs/x", 0), Ok(14));
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::ok(VFS_OP_STATX, 300, 0),
            &[],
        ));
        assert_eq!(vfs_statx_with(&mut t, b"/initramfs/x"), Ok(300));
        let mut t = ScriptTransport::new(Ok(())).reply(Scripted::completion(
            VfsCompletion::ok(VFS_OP_CLOSE, 0, 14),
            &[],
        ));
        assert_eq!(vfs_close_with(&mut t, 14), Ok(0));
        // An OPENAT completion carrying a capability is refused before the fd is returned.
        let mut r = Scripted::completion(VfsCompletion::ok(VFS_OP_OPENAT, 14, 0), &[]);
        r.transferred_cap = Some(9);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        assert_eq!(
            vfs_openat_with(&mut t, b"/initramfs/x", 0),
            Err(VfsClientError::Malformed(VfsCompletionFault::UnexpectedCap))
        );
        // A grant completion answering a CLOSE is another operation's reply.
        let mut r = Scripted::completion(VfsCompletion::ok(VFS_OP_FILE_GRANT_RO, 10, 14), &[]);
        r.transferred_cap = Some(9);
        let mut t = ScriptTransport::new(Ok(())).reply(r);
        assert_eq!(
            vfs_close_with(&mut t, 14),
            Err(VfsClientError::Malformed(VfsCompletionFault::WrongOp {
                expected: VFS_OP_CLOSE,
                got: VFS_OP_FILE_GRANT_RO
            }))
        );
    }

'''
s=s.replace(anchor, tests+anchor)
open(p,"w").write(s)
EOF
cd ../../.. && env -u CARGO_TARGET_DIR cargo test -q -p yarm-user-rt --lib -- vfs_client 2>&1 | tail -20