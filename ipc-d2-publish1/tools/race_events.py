#!/usr/bin/env python3
"""IPC-D2-PUBLISH1: attribute every D2_PUBLISH_RACE_UNWIND in a normalized boot log.

For each race line of receiver R it reports, from the log alone:
  block     the nearest preceding SCHED_BLOCK / D2_RECV_WAITER_TASK_BLOCKED of R (wait_gen)
  enqueue   IPCCALL_QUEUED_SPLIT_OK / *_QUEUED lines naming R's endpoint between R's IPC_RECV_ENTER
            and the race (sender tid, record index/generation)
  pair      the D2_RECV_WAITER_RACE_UNWIND of R that follows
  unwind    the following D2_RECV_SPLIT_UNWIND_OK of R (via, transition, priority)
  settle    the following IPC_RECV_BLOCK_SPLIT_SETTLED of R (reason, outcome)
  delivery  the first IPC_RECV_REPLY_CAP_RESPONDER_BOUND with responder R after the race, and
            whether its (reply_index, reply_gen) equals the queued record
  dup       any further RESPONDER_BOUND of the same record (a second delivery)
  failed    any IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED of R after the race
Log order across CPUs is print order, not proof of causal order; the report says so.
"""
import re, sys, json

def kv(line):
    return dict(re.findall(r"(\w+)=([^\s]+)", line))

def analyse(path):
    lines = open(path, errors="replace").read().split("\n")
    out = []
    for i, l in enumerate(lines):
        m = re.search(r"D2_PUBLISH_RACE_UNWIND endpoint=(\S+) tid=(\d+)", l)
        if not m:
            continue
        r = m.group(2)
        ev = {"line": i + 1, "route": m.group(1), "receiver": r}
        # receive entry and endpoint
        ent = None
        for j in range(i, max(0, i - 400), -1):
            if re.search(rf"IPC_RECV_AFTER_CAP_OK tid={r} ", lines[j]):
                ent = j; ev["endpoint"] = re.search(r"index: (\d+)", lines[j]).group(1); break
        for j in range(i, ent or i, -1):
            if re.search(rf"D2_RECV_WAITER_TASK_BLOCKED tid={r} ", lines[j]):
                ev["blocked_wait_gen"] = kv(lines[j]).get("wait_gen"); ev["blocked_line"] = j + 1; break
        enq = []
        for j in range((ent or i) - 40, i):
            if "QUEUED" in lines[j] and f"endpoint={ev.get('endpoint')} " in lines[j]:
                d = kv(lines[j]); enq.append({"line": j + 1, "marker": lines[j].split()[0],
                    "sender": d.get("tid"), "record": (d.get("record_index"), d.get("record_generation"))})
        ev["enqueue"] = enq
        win = lines[i + 1:i + 400]
        def first(pat):
            for k, x in enumerate(win):
                if re.search(pat, x):
                    return i + 2 + k, x
            return None, None
        _, p = first(rf"D2_RECV_WAITER_RACE_UNWIND tid={r}$|D2_RECV_WAITER_RACE_UNWIND tid={r}\b")
        ev["pair"] = bool(p)
        _, u = first(rf"D2_RECV_SPLIT_UNWIND_OK .*tid={r} ")
        ev["unwind"] = {k: kv(u).get(k) for k in ("via", "transition", "from", "status", "placement", "priority")} if u else None
        _, s = first(rf"IPC_RECV_BLOCK_SPLIT_SETTLED .*tid={r} ")
        ev["settle"] = {k: kv(s).get(k) for k in ("reason", "outcome")} if s else None
        dl, d = first(rf"IPC_RECV_REPLY_CAP_RESPONDER_BOUND .*responder_tid={r} ")
        if d:
            dd = kv(d); rec = (dd.get("reply_index"), dd.get("reply_gen"))
            ev["delivery"] = {"line": dl, "record": rec,
                              "matches_queued": any(e["record"] == rec for e in enq)}
            dup = [n + 1 for n, x in enumerate(lines) if n + 1 != dl and "RESPONDER_BOUND" in x
                   and f"reply_index={rec[0]} reply_gen={rec[1]} " in x]
            ev["other_bindings_of_record"] = dup
        else:
            ev["delivery"] = None
        if u:
            ev["receiver_cpu"] = kv(u).get("cpu")
        for e in enq:
            for x in lines[e["line"]:e["line"] + 40]:
                mm = re.search(r"YARM_LOCK_SPLIT_DISPATCH arch=\S+ nr=6 cpu=(\d+)", x)
                if mm:
                    e["sender_dispatch_cpu"] = mm.group(1); break
            rec = e["record"]
            e["reply_commit"] = any(f"record_index={rec[0]} record_generation={rec[1]} " in x
                                    and "TERMINAL_CLAIM" in x and "resolution=commit" in x for x in lines)
        ev["failed_closed_after"] = any(re.search(rf"IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED .*tid={r}\b", x) for x in lines[i:])
        out.append(ev)
    return out

if __name__ == "__main__":
    for p in sys.argv[1:]:
        print("==", p)
        for ev in analyse(p):
            print(json.dumps(ev))
