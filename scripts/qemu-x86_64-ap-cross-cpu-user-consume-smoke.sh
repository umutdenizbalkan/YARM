#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stage 199A2D2C2B3 — x86_64 AP saved-resume USER-MEMORY CONSUMPTION proof.
#
# Boots QEMU_SMP=2 with the cross-CPU request oracle and proves that the remotely-awakened CPU-1
# recv-v2 server, after the sealed saved-frame resume, executes NORMAL ring-3 loads and itself
# validates the delivered request payload + length + recv-v2 metadata + fresh receiver-local Reply cap
# — NO ring-3 fault after saved dispatch. Root-cause fix: EFER.NXE is now enabled on the AP (the NX
# bit on non-executable user data pages was previously treated as reserved, faulting every AP ring-3
# data read).
set -uo pipefail
cd "$(dirname "$0")/.."

FEATURE=x86-ipccall-direct-smp-oracle
KTARGET=${KTARGET:-targets/x86_64-yarm-none.json}
KPROFILE=${KPROFILE:-x86-none}
KELF=${KELF:-target/x86_64-yarm-none/${KPROFILE}/kernel_boot}
BUILD_STD=${BUILD_STD:-core,alloc,compiler_builtins,panic_abort}
LOGDIR=${LOGDIR:-/tmp/ap-cross-cpu-user}
# REGRADE=1 grades an existing "$LOGDIR/boot.log" without building or booting.
REGRADE=${REGRADE:-}
TIMEOUT_SECS=${TIMEOUT_SECS:-200}
mkdir -p "$LOGDIR"
BOOT_LOG="$LOGDIR/boot.log"

fail=0
note() { echo "[user-consume] $*"; }
die()  { echo "[user-consume][fail] $*"; fail=1; }

if [[ -z "$REGRADE" ]]; then
note "building base x86_64 artifacts"
BOOTSTRAP_FEATURE_ARGS="--no-default-features" \
  scripts/build-qemu-x86_64-artifacts.sh >"$LOGDIR/build.log" 2>&1 \
  || die "base artifact build failed (see $LOGDIR/build.log)"
if (( ! fail )); then
  note "rebuilding kernel_boot with --features $FEATURE"
  cargo build -Z "build-std=${BUILD_STD}" -Z json-target-spec \
    --target "$KTARGET" --profile "$KPROFILE" \
    --no-default-features --features "$FEATURE" \
    -p yarm --bin kernel_boot >"$LOGDIR/kbuild.log" 2>&1 \
    || die "feature kernel_boot build failed (see $LOGDIR/kbuild.log)"
fi
if (( ! fail )); then cp "$KELF" build-x86_64/kernel_boot.elf; fi
if (( fail )); then echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 result=fail reason=build"; exit 1; fi

note "booting QEMU -smp 2"
env \
  KERNEL_IMAGE=build-x86_64/kernel_boot.elf \
  INITRAMFS_IMAGE=build-x86_64/initramfs-core.cpio \
  KERNEL_CMDLINE="console=ttyS0 rdinit=/init yarm.x86_64_ipccall_direct_smp_oracle=1 yarm.x86_64_ipccall_direct_smp_recv_v2_server=1 yarm.x86_64_ipccall_direct_smp_request=1 yarm.ap_user_dispatch=1" \
  QEMU_SMP=2 \
  LOGFILE="$BOOT_LOG" \
  SMOKE_LOG="$LOGDIR/smoke.log" \
  TIMEOUT_SECS="$TIMEOUT_SECS" \
  YARM_MODE_ISOLATION=0 \
  scripts/qemu-x86_64-core-smoke.sh >"$LOGDIR/core-smoke.log" 2>&1 || true
fi

if [[ ! -s "$BOOT_LOG" ]]; then die "no boot log"; echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 result=fail reason=no_boot_log"; exit 1; fi
NORM="$LOGDIR/boot.norm.log"; tr '\r' '\n' <"$BOOT_LOG" >"$NORM"
count() { rg -a -c -F "$1" "$NORM" 2>/dev/null || echo 0; }
have()  { rg -a -q -F "$1" "$NORM"; }

# QEMU-SMP1-ACCEPTANCE §4 — the transaction is graded from the kernel's TRANSACTION RECORD, not from
# one-shot log lines: the production owners record each step once, with the exact identities they
# hold and a global step sequence, and the chain is reported SYNCHRONOUSLY (`printk_emit_sync`) once
# the resumed server's userspace validation is recorded. A line lost from the asynchronous printk
# ring can therefore neither fail a complete transaction nor stand in for a missing step. Checked:
#   * every step recorded exactly once (no duplicates), in causal order (the sender's post-ICR
#     `ipi_sent` only after the delivery: the target may answer the IPI before it is recorded);
#   * ONE server incarnation {tid, asid}, endpoint {index, generation} and acknowledgement seq from
#     the committed block through the delivery, the saved-frame resume and the ring-3 validation;
#   * a distinct requester, a CPU0 -> CPU1 remote wake, the IPI-driven wake taken on CPU 1, and at
#     least one hardware 0xF1 arrival on CPU 1 counted by the stub itself.
TXN_OUT="$(python3 - "$NORM" <<'PY'
import re, sys
lines = open(sys.argv[1], errors='replace').read().split('\n')
steps = ['blocked', 'delivered', 'ipi_sent', 'ipi_observed', 'resumed', 'continued', 'validated']
rec, seals, bad = {}, [], []
for l in lines:
    m = re.search(r'X86_SMP_REQUEST_TXN step=(\w+) seq=(\d+) f0=(\d+) f1=(\d+) f2=(\d+) f3=(\d+) f4=(\d+) f5=(\d+) dup=(\d+)', l)
    if m:
        if m.group(1) in rec: bad.append(f"step {m.group(1)} reported twice")
        rec[m.group(1)] = [int(x) for x in m.groups()[1:]]
    m = re.search(r'X86_SMP_REQUEST_TXN_SEAL (.*)', l)
    if m: seals.append(dict(re.findall(r'(\w+)=(\S+)', m.group(1))))
def need(cond, msg):
    if not cond: bad.append(msg)
need(len(seals) == 1, f"transaction seal count {len(seals)} != 1")
S = seals[0] if seals else {}
need(S.get('recorded') == '7' and S.get('duplicates') == '0', f"seal recorded={S.get('recorded')} duplicates={S.get('duplicates')}")
for st in steps:
    need(st in rec and rec[st][0] > 0, f"step {st} not recorded")
if not bad:
    seq = {st: rec[st][0] for st in steps}
    f = {st: rec[st][1:7] for st in steps}
    # `ipi_sent` is recorded when the sender's ICR write has returned, which the target may
    # already have answered: it follows the delivery it announces but is not ordered against the
    # target's own steps. Every other link is a real happens-before.
    chain = ['blocked', 'delivered', 'ipi_observed', 'resumed', 'continued', 'validated']
    need(all(seq[a] < seq[b] for a, b in zip(chain, chain[1:])) and seq['delivered'] < seq['ipi_sent'], f"causal order broken: {seq}")
    need(all(rec[st][7] == 0 for st in steps), "a step was recorded more than once")
    srv, asid, ep, ack = f['blocked'][0:4]
    need(srv != 0 and ack != 0, "blocked step lacks an identity")
    need(f['delivered'][0:4] == [srv, asid, ep, ack], f"delivery is another transaction's: {f['delivered'][0:4]} vs {[srv, asid, ep, ack]}")
    client, pair = f['delivered'][4], f['delivered'][5]
    sender, target = pair >> 8, pair & 0xFF
    need(client != 0 and client != srv, f"requester {client} is not a distinct task")
    need((sender, target) == (0, 1), f"wake {sender}->{target}, want 0->1")
    need(f['ipi_sent'][0:2] == [pair, srv], f"wake request is another transaction's: {f['ipi_sent'][0:2]}")
    need(f['ipi_observed'][0] == target, f"wake taken on cpu {f['ipi_observed'][0]}, want {target}")
    need(f['resumed'][0:3] == [target, srv, asid], f"resumed {f['resumed'][0:3]}, want cpu {target} {{{srv}, {asid}}}")
    need(f['continued'][0:2] == [srv, target], f"continuation by {f['continued'][0:2]}, want tid {srv} on cpu {target}")
    need(f['validated'][0:2] == [srv, target], f"validation by {f['validated'][0:2]}, want tid {srv} on cpu {target}")
    need(S.get('target_cpu') == str(target) and int(S.get('target_arrivals', 0)) >= 1, f"no hardware 0xF1 arrival counted on cpu {target}: {S}")
for b in bad: print(f"FAIL {b}")
if not bad:
    print(f"OK server_tid={srv} server_asid={asid} endpoint=0x{ep:x} ack_seq={ack} client_tid={client} cpu={sender}->{target} arrivals={S.get('target_arrivals')} order={'<'.join(str(seq[s]) for s in chain)} sent={seq['ipi_sent']}")
PY
)"
while IFS= read -r line; do
  case "$line" in
    FAIL\ *) die "transaction: ${line#FAIL }";;
    OK\ *) note "transaction record: ${line#OK }"; TXN_FACTS="${line#OK }";;
  esac
done <<<"$TXN_OUT"
[[ -n "${TXN_FACTS:-}" ]] || die "transaction record did not grade"

# Hard-stops: no ring-3 fault after resume, no validation failure, no migration/duplication.
[[ "$(count "X86_AP_RECV_V2_USER_READ_FAULT")" == "0" ]] || die "ring-3 user-read fault after resume"
[[ "$(count "X86_AP_RECV_V2_VALIDATE_FAIL")" == "0" ]] || die "server ring-3 validation failed"
have "X86_AP_RECV_V2_USER_VALIDATED cpu=0" && die "validation on wrong CPU (0)"
for bad in "KERNEL PANIC" "RUST PANIC" "panicked at" "DOUBLE FAULT" "Unhandled" "BOOTSTRAP_ERROR" "IPCCALL_DIRECT_ACK_OVERWRITE_FUSE"; do
  have "$bad" && die "fatal condition: $bad"
done

if (( fail )); then echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 result=fail (see $BOOT_LOG)"; exit 1; fi

note "genuine CPU-1 ring-3 user-memory consumption proven (payload+length+meta+reply-cap validated in userspace)"
echo "STAGE_199_IPCCALL_DIRECT_SMP_REQUEST_USER_SEAL arch=x86_64 smp=2 sender_cpu=0 receiver_cpu=1 cross_cpu=1 saved_resume=1 ring3_payload_read=1 ring3_metadata_read=1 ring3_reply_cap_read=1 duplicate_deliveries=0 duplicate_wakes=0 ${TXN_FACTS} result=ok"
