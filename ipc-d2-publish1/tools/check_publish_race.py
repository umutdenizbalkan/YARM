#!/usr/bin/env python3
"""IPC-D2-PUBLISH1 — PROTOTYPE of the proposed replacement for doc/AI_AGENT_RULES.md §14.3's
`d2_publish_race_unwinds` rule. It is NOT wired into any smoke or grader: the existing zero gate
stays in force until the rule owner decides. It exists to show the proposed rule is falsifiable,
by accepting the six saved VFS-CORRECT1C SMP2 events and rejecting every defect class.

Proposed rule, as checked here, for one normalized boot log:

  B  (broad route) The serialized broad arm's spelling, `D2_PUBLISH_RACE_UNWIND endpoint=<number>`,
     never appears. That arm runs phases A/B/C inside one `&mut KernelState` borrow, so a race
     there means the serialized section was violated. (It is also the only increment of the
     `d2_publish_race_unwinds` counter, which therefore stays 0.)

  S  (split route) Every `D2_PUBLISH_RACE_UNWIND endpoint=split tid=R` is a legal
     queue-publication race only if ALL of these hold, else it is a defect:
     S1 R blocked first: a `D2_RECV_WAITER_TASK_BLOCKED tid=R` after R's receive entry.
     S2 a message became visible on R's endpoint E: a queued enqueue naming endpoint E between R's
        receive entry and the race (`IPCCALL_QUEUED_SPLIT_OK`; other enqueue kinds are refused by
        this prototype as unsupported, which is conservative).
     S3 exactly one `D2_RECV_WAITER_RACE_UNWIND tid=R` pairs with it.
     S4 the unwind is exact and restored: `D2_RECV_SPLIT_UNWIND_OK ... tid=R ... via=exact ...
        priority=exact` and `IPC_RECV_BLOCK_SPLIT_SETTLED ... tid=R ... reason=queue_non_empty
        outcome=restored`, both before R's next receive entry.
     S5 single delivery: the raced record (reply_index, reply_gen) is bound to responder R exactly
        once in the whole boot, after the race.
     S6 the sender was answered: that record's reply terminal is claimed with `resolution=commit`.
     S7 no `IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED` names R anywhere after the race.

Usage: check_publish_race.py LOG...   |   check_publish_race.py --self-test LOG...
"""
import re
import sys


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s]+)", line))


def check(lines):
    fails, events = [], 0
    for i, l in enumerate(lines):
        m = re.search(r"D2_PUBLISH_RACE_UNWIND endpoint=(\S+) tid=(\d+)", l)
        if not m:
            continue
        events += 1
        route, r = m.group(1), m.group(2)
        tag = f"line {i + 1} tid={r}"
        if route != "split":
            fails.append(f"B: {tag}: broad-route race (endpoint={route})")
            continue
        ent = next((j for j in range(i, max(-1, i - 400), -1)
                    if re.search(rf"IPC_RECV_AFTER_CAP_OK tid={r} ", lines[j])), None)
        if ent is None:
            fails.append(f"S1: {tag}: no receive entry for the receiver")
            continue
        ep = re.search(r"index: (\d+)", lines[ent]).group(1)
        if not any(re.search(rf"D2_RECV_WAITER_TASK_BLOCKED tid={r} ", lines[j]) for j in range(ent, i)):
            fails.append(f"S1: {tag}: the receiver did not block before the race")
        enq = []
        for j in range(ent - 40, i):
            x = lines[j]
            if f"endpoint={ep} " in x and "QUEUED" in x and "REFUSED" not in x:
                if x.startswith("IPCCALL_QUEUED_SPLIT_OK"):
                    d = kv(x)
                    enq.append((d.get("record_index"), d.get("record_generation")))
                else:
                    fails.append(f"S2: {tag}: unsupported enqueue kind {x.split()[0]}")
        if not enq:
            fails.append(f"S2: {tag}: no message became visible on endpoint {ep} (spurious QueueNonEmpty)")
        nxt = next((j for j in range(i + 1, len(lines))
                    if re.search(rf"IPC_RECV_AFTER_CAP_OK tid={r} |IPC_RECV_ENTER tid={r} ", lines[j])), len(lines))
        seg = lines[i + 1:nxt]
        pairs = [x for x in seg if re.fullmatch(rf"D2_RECV_WAITER_RACE_UNWIND tid={r}", x.strip())]
        if len(pairs) != 1:
            fails.append(f"S3: {tag}: {len(pairs)} paired D2_RECV_WAITER_RACE_UNWIND lines")
        unw = [x for x in seg if x.startswith("D2_RECV_SPLIT_UNWIND_OK") and f" tid={r} " in x]
        if not (len(unw) == 1 and " via=exact " in unw[0] and " priority=exact" in unw[0]):
            fails.append(f"S4: {tag}: unwind not exact ({unw})")
        st = [x for x in seg if x.startswith("IPC_RECV_BLOCK_SPLIT_SETTLED") and f" tid={r} " in x]
        if not (len(st) == 1 and "reason=queue_non_empty " in st[0] and st[0].rstrip().endswith("outcome=restored")):
            fails.append(f"S4: {tag}: settle not queue_non_empty/restored ({st})")
        for rec in enq:
            bound = [j for j in range(len(lines)) if "IPC_RECV_REPLY_CAP_RESPONDER_BOUND" in lines[j]
                     and f"reply_index={rec[0]} reply_gen={rec[1]} " in lines[j]]
            mine = [j for j in bound if j > i and f"responder_tid={r} " in lines[j]]
            if len(bound) != 1 or len(mine) != 1:
                fails.append(f"S5: {tag}: record {rec} bound {len(bound)}x, {len(mine)}x to the receiver after the race")
            if not any("TERMINAL_CLAIM" in x and f"record_index={rec[0]} record_generation={rec[1]} " in x
                       and "resolution=commit" in x for x in lines):
                fails.append(f"S6: {tag}: record {rec} never answered (no committed reply terminal)")
        if any(re.search(rf"IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED .*tid={r}\b", x) for x in lines[i:]):
            fails.append(f"S7: {tag}: fail-closed receive after the race")
    return events, fails


def load(p):
    return open(p, errors="replace").read().replace("\r", "\n").split("\n")


def self_test(paths):
    total = 0
    for p in paths:
        base = load(p)
        n, f = check(base)
        assert n > 0 and not f, (p, n, f)
        total += n
        race = next(i for i, x in enumerate(base) if x.startswith("D2_PUBLISH_RACE_UNWIND"))

        def mut(fn, rule, why):
            lines = list(base)
            fn(lines)
            _, ff = check(lines)
            assert any(x.startswith(rule + ":") for x in ff), (p, why, ff)

        def idx(pred, after=race):
            return next(i for i in range(after, len(base)) if pred(base[i]))

        def drop(pred, after=race):
            def f(ls):
                del ls[idx(pred, after)]
            return f

        def sub(pred, a, b, after=race):
            def f(ls):
                j = idx(pred, after)
                ls[j] = ls[j].replace(a, b)
            return f
        enq = next(i for i in range(race, 0, -1) if base[i].startswith("IPCCALL_QUEUED_SPLIT_OK"))
        bound = idx(lambda x: "IPC_RECV_REPLY_CAP_RESPONDER_BOUND" in x)
        mut(sub(lambda x: x.startswith("D2_PUBLISH_RACE_UNWIND"), "endpoint=split", "endpoint=10"), "B", "broad spelling")
        mut(drop(lambda x: x.startswith("D2_RECV_WAITER_RACE_UNWIND")), "S3", "unpaired")
        mut(lambda ls: ls.insert(race + 1, ls[idx(lambda x: x.startswith("D2_RECV_WAITER_RACE_UNWIND"))]), "S3", "double pair")
        mut(sub(lambda x: x.startswith("D2_RECV_SPLIT_UNWIND_OK"), "via=exact", "via=fallback"), "S4", "not exact")
        mut(sub(lambda x: x.startswith("IPC_RECV_BLOCK_SPLIT_SETTLED"), "outcome=restored", "outcome=unwound"), "S4", "not restored")
        mut(lambda ls: ls.__delitem__(enq), "S2", "no visible message")
        mut(lambda ls: ls.__delitem__(bound), "S5", "lost delivery")
        mut(lambda ls: ls.insert(bound + 1, ls[bound]), "S5", "double delivery")
        mut(lambda ls: [ls.__setitem__(j, "") for j in range(len(ls)) if "TERMINAL_CLAIM" in ls[j]
                        and "record_index=" + base[enq].split("record_index=")[1].split()[0] + " " in ls[j]
                        and "record_generation=" + base[enq].split("record_generation=")[1].split()[0] + " " in ls[j]],
            "S6", "sender never answered")
        mut(lambda ls: ls.insert(race + 3, "IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu=0 tid=10002 reason=x"), "S7", "fail closed")
        mut(lambda ls: [ls.__setitem__(j, "") for j in range(race - 1, race - 60, -1)
                        if ls[j].startswith("D2_RECV_WAITER_TASK_BLOCKED tid=10002 ")], "S1", "no block")
    print(f"CHECK_PUBLISH_RACE_SELF_TEST result=ok logs={len(paths)} events={total} mutants_per_log=11")


if __name__ == "__main__":
    a = sys.argv[1:]
    if a and a[0] == "--self-test":
        self_test(a[1:])
        sys.exit(0)
    rc = 0
    for p in a:
        n, f = check(load(p))
        print(f"CHECK_PUBLISH_RACE log={p} events={n} result={'ok' if not f else 'fail'}")
        for x in f:
            print("  FAIL " + x)
        rc |= bool(f)
    sys.exit(rc)
