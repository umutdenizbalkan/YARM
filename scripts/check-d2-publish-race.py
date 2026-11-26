#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""IPC-D2-RULE1 — the route-specific `D2_PUBLISH_RACE_UNWIND` rule (doc/AI_AGENT_RULES.md §14.3).

This is the ONE checker every gate uses for the marker: the three core smokes, the two
optional-fs smokes and the fd witness grader call it. A boot gets one of three verdicts.

  ok          no publish race, or every split race fully attributed and recovered
  defect      stop-ship: a broken invariant, contradictory evidence, or a missing obligation
  unverified  the trace cannot attribute a race one-to-one (e.g. records were lost, or the raced
              message carries no identity on the receiving side); also a failed gate

Exit status: 0 ok, 1 defect, 2 unverified. Usage:
  check-d2-publish-race.py [--quiet] LOG...       (raw or normalized boot logs)
  check-d2-publish-race.py --self-test [FIXTURE...]   (scripts/fixtures/d2-publish-race/*.txt)

Ordering. One global printk ring; every record takes its sequence with one `fetch_add`, and the
console drains in sequence order. So the records ONE thread emits appear in that thread's program
order, and nothing else does: two CPUs' lines interleave in print order, which is not causal
order. The checker therefore uses
  * the receiver's own program order, for the markers its own syscall path emits
    (`IPC_RECV_ENTER` / `IPC_RECV_AFTER_CAP_OK`, `D2_RECV_WAITER_TASK_BLOCKED`, the race pair,
    `D2_RECV_SPLIT_UNWIND_OK`, `IPC_RECV_BLOCK_SPLIT_SETTLED`), and
  * identity across CPUs: a queued call's reply record (index, generation) links the sender's
    `IPCCALL_QUEUED_SPLIT_OK`, the receiver's `IPC_RECV_REPLY_CAP_RESPONDER_BOUND` and the replier's
    terminal commit (S6).
Proximity and cross-CPU print order are never used to identify a wait or a message.

Why the recovery delivery is the receiver's FIRST `RESPONDER_BOUND` after the restored settle and
before its next wait or receive: between the unwind and its next publication the receiver is not a
published waiter, so no sender can deliver to it directly; only its own immediate take binds it.
A direct delivery for its NEXT wait happens after that wait's publication, which follows (in the
receiver's program order, through the publication lock) its `D2_RECV_WAITER_TASK_BLOCKED` record.

THE RULE.
  B   Broad route. `D2_PUBLISH_RACE_UNWIND endpoint=<index>` (the serialized broad arm, the only
      increment of `d2_publish_race_unwinds`) is stop-ship. So is a race line naming no route.
  F   `IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED`, anywhere, is stop-ship, independently of any race.
  D   `D2_RECV_WAITER_DISPLACED` naming a different receiver (old_tid != new_tid) is stop-ship
      (IPC-DISPLACE1), independently of any race. The same receiver superseding itself is not.
  S   Each `D2_PUBLISH_RACE_UNWIND endpoint=split tid=R`, in R's program order:
      S1 consumes exactly one wait of R's current receive on endpoint E
         (`D2_RECV_WAITER_TASK_BLOCKED tid=R`) — no wait, or a wait an earlier race already
         consumed (a duplicate publication sharing one recovery), is a defect;
      S3 has exactly one companion `D2_RECV_WAITER_RACE_UNWIND tid=R` before its settle;
      S4 has exactly one `D2_RECV_SPLIT_UNWIND_OK tid=R … via=exact … priority=exact` and exactly
         one `IPC_RECV_BLOCK_SPLIT_SETTLED tid=R endpoint=E reason=queue_non_empty
         outcome=restored`;
      S5 its delivery: R's first `RESPONDER_BOUND` after that settle and before R's next wait or
         receive names record (i, g); that record has exactly one `IPCCALL_QUEUED_SPLIT_OK`,
         naming E (index and generation), and exactly one `RESPONDER_BOUND` in the boot; and no
         other race is attributed to it;
      S6 a queued CALL owes a reply: exactly one terminal commit for (i, g). A direct reply commits
         its terminal in one of two places, both through `commit_direct_reply_terminal_split`:
         a payload reply in the syscall (`IPCREPLY_DIRECT_TERMINAL_CLAIM … resolution=commit`), a
         capability-bearing reply in the post-work settle once its copy is delivered
         (`IPC_REPLY_DIRECT_CAP_SETTLED … terminal_committed=1`, DIRECT3-CAP-FINAL). Either counts
         once; a settle with `terminal_committed=0` is no commit. The reply requirement applies
         only to operations that owe one.
  O   Orphans, both directions: a companion with no race, and a `queue_non_empty` settle with no
      race, are defects.
  U   Unverified rather than defect, because the trace cannot decide:
      * the delivered record has no `IPCCALL_QUEUED_SPLIT_OK` (queued by a route that does not
        name it);
      * no record was bound in the delivery window, but a message that owes no reply (a queued
        send or reply) was enqueued on E: its delivery carries no identity on the receiving side;
      * a call's only terminal evidence is not a commit;
      * printk reported lost records (`PRINTK_RECORDS_LOST`), and an obligation is missing
        rather than contradicted.
"""
import re
import sys

RACE = re.compile(r"\bD2_PUBLISH_RACE_UNWIND\b(.*)$")
PAIR = re.compile(r"\bD2_RECV_WAITER_RACE_UNWIND tid=(\d+)\b")


def kv(line):
    return dict(re.findall(r"(\w+)=([^\s]+)", line))


def norm(text):
    return [l.rstrip() for l in text.replace("\r", "\n").split("\n")]


class Report:
    def __init__(self):
        self.events = []      # dicts: line, receiver, route, verdict, notes, record
        self.defects = []
        self.unverified = []
        self.lost = False

    def verdict(self):
        if self.defects:
            return "defect"
        if self.unverified:
            return "unverified"
        return "ok"


def check_lines(lines):
    rep = Report()
    rep.lost = any("PRINTK_RECORDS_LOST" in l for l in lines)

    def defect(msg, missing=False):
        # A missing obligation in a lossy trace cannot be decided; contradictions always can.
        (rep.unverified if (missing and rep.lost) else rep.defects).append(msg)

    # ── independent failures ──
    for n, l in enumerate(lines, 1):
        if "IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED" in l:
            rep.defects.append(f"F: line {n}: {l.strip()[:120]}")
        if "D2_RECV_WAITER_DISPLACED" in l:
            d = kv(l)
            if d.get("old_tid") != d.get("new_tid"):
                rep.defects.append(f"D: line {n}: another receiver displaced (old_tid={d.get('old_tid')} new_tid={d.get('new_tid')})")

    # ── identity tables (cross-CPU) ──
    enq = {}      # record -> list of (line, endpoint, endpoint_generation, sender)
    bound = {}    # record -> list of (line, responder)
    commits = {}  # record -> list of lines
    releases = {}
    noreply_enq = {}  # endpoint -> lines (queued sends / replies: owe no reply)
    for n, l in enumerate(lines, 1):
        s = l.strip()
        if s.startswith("IPCCALL_QUEUED_SPLIT_OK"):
            d = kv(s)
            enq.setdefault((d.get("record_index"), d.get("record_generation")), []).append(
                (n, d.get("endpoint"), d.get("endpoint_generation"), d.get("tid")))
        elif s.startswith("IPC_RECV_REPLY_CAP_RESPONDER_BOUND"):
            d = kv(s)
            bound.setdefault((d.get("reply_index"), d.get("reply_gen")), []).append((n, d.get("responder_tid")))
        elif s.startswith("IPCREPLY_DIRECT_TERMINAL_CLAIM"):
            d = kv(s)
            key = (d.get("record_index"), d.get("record_generation"))
            (commits if d.get("resolution") == "commit" else releases).setdefault(key, []).append(n)
        elif s.startswith("IPC_REPLY_DIRECT_CAP_SETTLED"):
            # VFS-LIFETIME1: a capability-bearing direct reply commits its terminal here, not in
            # the syscall (S6). Only a committed settle counts.
            d = kv(s)
            if d.get("terminal_committed") == "1":
                commits.setdefault((d.get("record_index"), d.get("record_generation")), []).append(n)
        elif s.startswith("IPC_SEND_SPLIT_DONE") and "result=enqueued" in s:
            noreply_enq.setdefault(kv(s).get("endpoint"), []).append(n)
        elif s.startswith("IPC_REPLY_QUEUED_SPLIT_OK"):
            noreply_enq.setdefault(kv(s).get("endpoint"), []).append(n)

    # ── per-receiver program order ──
    receivers = set()
    for l in lines:
        m = RACE.search(l)
        if m:
            t = kv(m.group(1)).get("tid")
            if t:
                receivers.add(t)
        m = PAIR.search(l)
        if m:
            receivers.add(m.group(1))
        if "IPC_RECV_BLOCK_SPLIT_SETTLED" in l and "reason=queue_non_empty" in l:
            receivers.add(kv(l).get("tid"))

    # broad / malformed race lines
    for n, l in enumerate(lines, 1):
        m = RACE.search(l)
        if not m:
            continue
        d = kv(m.group(1))
        if d.get("endpoint") != "split":
            rep.defects.append(f"B: line {n}: broad-route or malformed race ({l.strip()[:80]})")
            rep.events.append({"line": n, "receiver": d.get("tid"), "route": d.get("endpoint"), "verdict": "defect"})

    attributed = {}  # record -> race line
    for r in sorted(receivers, key=lambda x: int(x) if x and x.isdigit() else 0):
        own = []
        for n, l in enumerate(lines, 1):
            s = l.strip()
            if re.match(rf"(IPC_RECV_ENTER|IPC_RECV_AFTER_CAP_OK) tid={r} ", s):
                own.append(("entry", n, s))
            elif re.match(rf"D2_RECV_WAITER_TASK_BLOCKED tid={r} ", s):
                own.append(("wait", n, s))
            elif RACE.search(s) and kv(RACE.search(s).group(1)).get("tid") == r and \
                    kv(RACE.search(s).group(1)).get("endpoint") == "split":
                own.append(("race", n, s))
            elif re.fullmatch(rf".*\bD2_RECV_WAITER_RACE_UNWIND tid={r}", s):
                own.append(("pair", n, s))
            elif s.startswith("D2_RECV_SPLIT_UNWIND_OK") and f" tid={r} " in s:
                own.append(("unwind", n, s))
            elif s.startswith("IPC_RECV_BLOCK_SPLIT_SETTLED") and f" tid={r} " in s:
                own.append(("settle", n, s))
            elif s.startswith("IPC_RECV_REPLY_CAP_RESPONDER_BOUND") and f" responder_tid={r} " in s:
                own.append(("bound", n, s))
        recv_ep = None          # (index, generation) of the current receive
        wait = None             # (line, gen, consumed)
        race = None             # open race context
        last_race_for_pair = None
        pending_delivery = None
        for kind, n, s in own:
            if pending_delivery is not None and kind in ("entry", "wait", "race"):
                _close_delivery(rep, pending_delivery, None, enq, bound, commits, releases,
                                noreply_enq, attributed, defect)
                pending_delivery = None
            if kind == "entry":
                m = re.search(r"index: (\d+), generation: (\d+)", s)
                if m:
                    recv_ep = (m.group(1), m.group(2))
                    wait = None
                if race is not None:
                    defect(f"S4: line {race['line']} tid={r}: race has no settle before the next receive", missing=True)
                    race = None
            elif kind == "wait":
                wait = {"line": n, "gen": kv(s).get("wait_gen"), "consumed": False}
            elif kind == "race":
                ev = {"line": n, "receiver": r, "route": "split", "endpoint": recv_ep,
                      "pairs": 0, "unwinds": [], "settles": []}
                rep.events.append(ev)
                if race is not None:
                    defect(f"S4: line {race['line']} tid={r}: race has no settle before the next race", missing=True)
                if wait is None:
                    defect(f"S1: line {n} tid={r}: race with no wait of the receiver's current receive", missing=True)
                elif wait["consumed"]:
                    rep.defects.append(f"S1: line {n} tid={r}: wait gen {wait['gen']} (line {wait['line']}) already "
                                       "consumed by an earlier race — a duplicate publication sharing one recovery")
                else:
                    wait["consumed"] = True
                    ev["wait_gen"] = wait["gen"]
                race = ev
                last_race_for_pair = ev
            elif kind == "pair":
                if race is None or race["pairs"] >= 1:
                    tgt = race if race is not None else last_race_for_pair
                    if race is None:
                        rep.defects.append(f"O: line {n} tid={r}: companion D2_RECV_WAITER_RACE_UNWIND with no open race")
                    else:
                        rep.defects.append(f"S3: line {n} tid={r}: a second companion for the race at line {tgt['line']}")
                else:
                    race["pairs"] += 1
            elif kind == "unwind":
                if race is not None:
                    race["unwinds"].append(s)
            elif kind == "settle":
                d = kv(s)
                reason = d.get("reason", "")
                if reason.startswith("queue_non_empty"):
                    if race is None:
                        rep.defects.append(f"O: line {n} tid={r}: a {reason} settle with no race")
                        continue
                    race["settles"].append(s)
                    if race["pairs"] != 1:
                        defect(f"S3: line {race['line']} tid={r}: {race['pairs']} companions before the settle",
                               missing=race["pairs"] == 0)
                    if len(race["unwinds"]) != 1:
                        defect(f"S4: line {race['line']} tid={r}: {len(race['unwinds'])} exact-unwind lines",
                               missing=len(race["unwinds"]) == 0)
                    elif " via=exact " not in race["unwinds"][0] or " priority=exact" not in race["unwinds"][0]:
                        rep.defects.append(f"S4: line {race['line']} tid={r}: unwind not exact ({race['unwinds'][0][:90]})")
                    if reason != "queue_non_empty" or d.get("outcome") != "restored":
                        rep.defects.append(f"S4: line {race['line']} tid={r}: settled {reason}/{d.get('outcome')}, not "
                                           "queue_non_empty/restored")
                    if recv_ep is not None and d.get("endpoint") != recv_ep[0]:
                        rep.defects.append(f"S4: line {race['line']} tid={r}: settled on endpoint {d.get('endpoint')}, "
                                           f"the receive was on {recv_ep[0]}")
                    pending_delivery = race
                    race = None
                elif race is not None:
                    rep.defects.append(f"S4: line {race['line']} tid={r}: settled {reason}, not queue_non_empty")
                    race = None
            elif kind == "bound":
                if pending_delivery is not None:
                    d = kv(s)
                    _close_delivery(rep, pending_delivery, (d.get("reply_index"), d.get("reply_gen"), n),
                                    enq, bound, commits, releases, noreply_enq, attributed, defect)
                    pending_delivery = None
        if pending_delivery is not None:
            _close_delivery(rep, pending_delivery, None, enq, bound, commits, releases, noreply_enq,
                            attributed, defect)
        if race is not None:
            defect(f"S4: line {race['line']} tid={r}: race never settled", missing=True)
    return rep


def _close_delivery(rep, ev, rec, enq, bound, commits, releases, noreply_enq, attributed, defect):
    r, ep = ev["receiver"], ev.get("endpoint")
    if rec is None:
        others = noreply_enq.get(ep[0] if ep else None, [])
        if others:
            rep.unverified.append(f"U: line {ev['line']} tid={r}: no record bound in the delivery window; a message "
                                  f"that owes no reply was queued on endpoint {ep[0]} (lines {others[:4]}), and its "
                                  "delivery carries no identity")
        else:
            defect(f"S5: line {ev['line']} tid={r}: no delivery attributable to the race", missing=True)
        return
    key = (rec[0], rec[1])
    ev["record"] = key
    if key in attributed:
        rep.defects.append(f"S5: line {ev['line']} tid={r}: record {key} already attributed to the race at line "
                           f"{attributed[key]}")
    attributed[key] = ev["line"]
    b = bound.get(key, [])
    if len(b) != 1:
        rep.defects.append(f"S5: line {ev['line']} tid={r}: record {key} bound {len(b)} times in the boot")
    e = enq.get(key, [])
    if not e:
        rep.unverified.append(f"U: line {ev['line']} tid={r}: record {key} has no IPCCALL_QUEUED_SPLIT_OK; the "
                              "message on the endpoint cannot be identified")
        return
    if len(e) != 1:
        rep.defects.append(f"S5: line {ev['line']} tid={r}: record {key} enqueued {len(e)} times")
    if ep is not None and (e[0][1], e[0][2]) != ep:
        rep.defects.append(f"S5: line {ev['line']} tid={r}: record {key} was queued on endpoint {e[0][1]}/{e[0][2]}, "
                           f"the receive was on {ep[0]}/{ep[1]}")
    ev["sender"] = e[0][3]
    c = commits.get(key, [])
    if len(c) > 1:
        rep.defects.append(f"S6: line {ev['line']} tid={r}: record {key} committed {len(c)} times (lines {c})")
    elif not c:
        if releases.get(key):
            rep.unverified.append(f"U: line {ev['line']} tid={r}: record {key} released (lines {releases[key]}) and "
                                  "never committed")
        else:
            defect(f"S6: line {ev['line']} tid={r}: record {key} owes a reply and none was committed", missing=True)
    else:
        ev["recovered"] = True


def summary(rep):
    split = [e for e in rep.events if e.get("route") == "split"]
    rec = sum(1 for e in split if e.get("recovered"))
    return (f"D2_PUBLISH_RACE_CHECK verdict={rep.verdict()} races={len(rep.events)} split={len(split)} "
            f"broad={len(rep.events) - len(split)} recovered={rec} defects={len(rep.defects)} "
            f"unverified={len(rep.unverified)} lost_records={int(rep.lost)}")


def check_text(text):
    return check_lines([l for l in norm(text) if not l.startswith("#")])


# ── self-test ──────────────────────────────────────────────────────────────────────────────────

def _synthetic(two_on_same_endpoint=True):
    """Two queued calls to one endpoint, each raced and recovered, interleaved across CPUs: S2's
    enqueue prints before R's first race, S1's after it. Each race must bind its own record."""
    return [
        "IPC_RECV_AFTER_CAP_OK tid=100 cap=7 endpoint=Endpoint { index: 4, generation: 2 }",
        "D2_RECV_WAITER_TASK_BLOCKED tid=100 wait_gen=11",
        "IPCCALL_QUEUED_SPLIT_OK tid=201 endpoint=4 endpoint_generation=2 reply_cap=9 record_index=3 record_generation=50 len=8 result=ok",
        "IPCCALL_QUEUED_SPLIT_OK tid=202 endpoint=4 endpoint_generation=2 reply_cap=9 record_index=5 record_generation=60 len=8 result=ok",
        "D2_PUBLISH_RACE_UNWIND endpoint=split tid=100",
        "D2_RECV_WAITER_RACE_UNWIND tid=100",
        "D2_RECV_SPLIT_UNWIND_OK cpu=0 tid=100 asid=3 via=exact transition=dispatch_incoming from=Runnable status=running placement=current priority=exact",
        "IPC_RECV_BLOCK_SPLIT_SETTLED cpu=0 tid=100 endpoint=4 reason=queue_non_empty outcome=restored",
        "IPC_RECV_REPLY_CAP_RESPONDER_BOUND reply_index=3 reply_gen=50 responder_tid=100 responder_asid=3 result=ok",
        "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=3 record_generation=50 replier_tid=100 terminal=Reply resolution=commit settled=1 result=ok",
        "IPC_RECV_AFTER_CAP_OK tid=100 cap=7 endpoint=Endpoint { index: 4, generation: 2 }",
        "D2_RECV_WAITER_TASK_BLOCKED tid=100 wait_gen=12",
        "D2_PUBLISH_RACE_UNWIND endpoint=split tid=100",
        "D2_RECV_WAITER_RACE_UNWIND tid=100",
        "D2_RECV_SPLIT_UNWIND_OK cpu=0 tid=100 asid=3 via=exact transition=dispatch_incoming from=Runnable status=running placement=current priority=exact",
        "IPC_RECV_BLOCK_SPLIT_SETTLED cpu=0 tid=100 endpoint=4 reason=queue_non_empty outcome=restored",
        "IPC_RECV_REPLY_CAP_RESPONDER_BOUND reply_index=5 reply_gen=60 responder_tid=100 responder_asid=3 result=ok",
        "IPCREPLY_DIRECT_TERMINAL_CLAIM record_index=5 record_generation=60 replier_tid=100 terminal=Reply resolution=commit settled=1 result=ok",
    ]


def is_commit(line):
    """S6's two commit markers (see the rule)."""
    return (line.startswith("IPCREPLY_DIRECT_TERMINAL_CLAIM") and "resolution=commit" in line) or \
        (line.startswith("IPC_REPLY_DIRECT_CAP_SETTLED") and "terminal_committed=1" in line)


def self_test(fixtures):
    count = 0

    def expect(lines, verdict, rule, why):
        nonlocal count
        rep = check_lines(lines)
        got = rep.verdict()
        msgs = rep.defects if verdict == "defect" else rep.unverified
        assert got == verdict and (rule is None or any(m.startswith(rule + ":") for m in msgs)), \
            (why, got, rep.defects, rep.unverified)
        count += 1

    # (1) interleaved calls to one endpoint: each race's evidence is its own
    syn = _synthetic()
    expect(syn, "ok", None, "interleaved calls pass")
    rep = check_lines(syn)
    assert [e.get("record") for e in rep.events] == [("3", "50"), ("5", "60")], rep.events
    j = lambda p: next(i for i, x in enumerate(syn) if p(x))
    c2 = j(lambda x: "record_index=5 record_generation=60" in x and "TERMINAL_CLAIM" in x)
    expect(syn[:c2] + syn[c2 + 1:], "defect", "S6", "call 2 unanswered; call 1's commit must not satisfy it")
    b2 = j(lambda x: "reply_index=5 reply_gen=60" in x)
    expect(syn[:b2] + [syn[b2].replace("reply_index=5 reply_gen=60", "reply_index=3 reply_gen=50")] + syn[b2 + 1:],
           "defect", "S5", "race 2 claims call 1's record")
    e2 = j(lambda x: "record_index=5 record_generation=60" in x and "QUEUED" in x)
    expect(syn[:e2] + [syn[e2].replace("endpoint=4 ", "endpoint=9 ")] + syn[e2 + 1:], "defect", "S5",
           "call 2 queued on another endpoint")
    expect(syn[:e2] + syn[e2 + 1:], "unverified", "U", "call 2's message not identifiable")
    w2 = j(lambda x: "wait_gen=12" in x)
    expect(syn[:w2] + syn[w2 + 1:], "defect", "S1", "race 2 without its own wait")
    # (2) the new controls
    r1 = j(lambda x: x.startswith("D2_PUBLISH_RACE_UNWIND"))
    expect(syn[:r1 + 2] + syn[r1:r1 + 2] + syn[r1 + 2:], "defect", "S1",
           "duplicate publication sharing one recovery")
    expect(syn + ["D2_RECV_WAITER_RACE_UNWIND tid=100"], "defect", "O", "orphan companion")
    expect(syn + ["IPC_RECV_BLOCK_SPLIT_SETTLED cpu=0 tid=100 endpoint=4 reason=queue_non_empty outcome=restored"],
           "defect", "O", "orphan settle")
    c1 = j(lambda x: "record_index=3 record_generation=50" in x and "TERMINAL_CLAIM" in x)
    expect(syn[:c1 + 1] + [syn[c1]] + syn[c1 + 1:], "defect", "S6", "duplicate terminal commit")
    expect(syn + ["D2_RECV_WAITER_DISPLACED endpoint=4 old_tid=100 old_asid=3 old_wait_gen=12 new_tid=300 new_asid=4 new_wait_gen=1"],
           "defect", "D", "another receiver displaced")
    expect(syn + ["D2_RECV_WAITER_DISPLACED endpoint=4 old_tid=100 old_asid=3 old_wait_gen=11 new_tid=100 new_asid=3 new_wait_gen=12"],
           "ok", None, "self-supersession is not displacement")
    expect(["IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu=0 tid=5 phase=publish outcome=x reason=y"], "defect", "F",
           "fail-closed without any race")
    expect(["D2_PUBLISH_RACE_UNWIND endpoint=10 tid=3", "D2_RECV_WAITER_RACE_UNWIND tid=3"], "defect", "B", "broad race")
    expect(["D2_PUBLISH_RACE_UNWIND tid=3"], "defect", "B", "race naming no route")
    c1r = syn[c1].replace("resolution=commit", "resolution=release")
    expect(syn[:c1] + [c1r] + syn[c1 + 1:], "unverified", "U", "call released, never committed")
    # a message that owes no reply: a queued send, and no record bound
    send = [x for x in syn[:9] if "QUEUED" not in x] + \
        ["IPC_SEND_SPLIT_DONE cpu=1 tid=201 endpoint=4 result=enqueued"]
    expect(send, "unverified", "U", "queued send: delivery has no identity, no reply owed")
    expect([x for x in syn[:8] if "QUEUED" not in x], "defect", "S5", "race with nothing on the endpoint")
    lossy = ["PRINTK_RECORDS_LOST first_seq=5 count=2 total_dropped=2"] + [x for x in syn if x != "D2_RECV_WAITER_RACE_UNWIND tid=100"]
    expect(lossy, "unverified", "S3", "lost records: a missing companion is undecidable")
    expect(["noise"], "ok", None, "no race at all")
    # VFS-LIFETIME1: a capability-bearing reply commits in its post-work settle, not in the syscall
    cap2 = ("IPC_REPLY_DIRECT_CAP_SETTLED record_index=5 record_generation=60 replier_tid=100 caller_tid=202 "
            "consumed=1 replier_revoked=1 caller_revoked=1 terminal_committed=1 result=ok")
    expect(syn[:c2] + [cap2] + syn[c2 + 1:], "ok", None, "call 2 answered by a capability-bearing reply")
    expect(syn[:c2] + [cap2.replace("terminal_committed=1", "terminal_committed=0")] + syn[c2 + 1:], "defect", "S6",
           "a settle that did not commit the terminal is no reply")
    expect(syn + [cap2], "defect", "S6", "a payload commit and a capability settle for one record")
    expect(syn[:c2] + [cap2.replace("record_generation=60", "record_generation=61")] + syn[c2 + 1:], "defect", "S6",
           "a capability settle of another record must not answer call 2")
    n_syn = count

    # (3) the 11 retained controls of the IPC-D2-PUBLISH1 prototype, on each archived fixture
    for p in fixtures:
        base = [l for l in norm(open(p, errors="replace").read()) if not l.startswith("#")]
        rep = check_lines(base)
        assert rep.verdict() == "ok" and rep.events and all(e.get("recovered") for e in rep.events), \
            (p, rep.defects, rep.unverified)
        race = next(i for i, x in enumerate(base) if x.startswith("D2_PUBLISH_RACE_UNWIND"))
        at = lambda pred, after=race: next(i for i in range(after, len(base)) if pred(base[i]))
        enq = next(i for i in range(race, -1, -1) if base[i].startswith("IPCCALL_QUEUED_SPLIT_OK"))
        rec = (kv(base[enq])["record_index"], kv(base[enq])["record_generation"])
        bnd = at(lambda x: x.startswith("IPC_RECV_REPLY_CAP_RESPONDER_BOUND") and
                 f"reply_index={rec[0]} reply_gen={rec[1]} " in x)
        r = kv(base[race]).get("tid")

        def edit(i, a, b):
            ls = list(base)
            ls[i] = ls[i].replace(a, b)
            return ls

        def drop(i):
            return base[:i] + base[i + 1:]
        pair = at(lambda x: x.startswith("D2_RECV_WAITER_RACE_UNWIND"))
        unw = at(lambda x: x.startswith("D2_RECV_SPLIT_UNWIND_OK"))
        stl = at(lambda x: x.startswith("IPC_RECV_BLOCK_SPLIT_SETTLED"))
        wt = next(i for i in range(race, -1, -1) if base[i].startswith(f"D2_RECV_WAITER_TASK_BLOCKED tid={r} "))
        ctl = [
            (edit(race, "endpoint=split", "endpoint=10"), "defect", "B", "broad spelling"),
            (drop(pair), "defect", "S3", "unpaired"),
            (base[:pair] + [base[pair]] + base[pair:], "defect", "S3", "double pair"),
            (edit(unw, "via=exact", "via=fallback"), "defect", "S4", "not exact"),
            (edit(stl, "outcome=restored", "outcome=unwound"), "defect", "S4", "not restored"),
            (drop(enq), "unverified", "U", "the message on the endpoint removed"),
            (drop(bnd), "defect", None, "delivery removed"),
            (base[:bnd + 1] + [base[bnd]] + base[bnd + 1:], "defect", "S5", "double delivery"),
            ([x for x in base if not (is_commit(x) and f"record_index={rec[0]} record_generation={rec[1]} " in x)],
             "defect", "S6", "sender never answered"),
            (base[:race + 3] + [f"IPC_RECV_BLOCK_SPLIT_FAILED_CLOSED cpu=0 tid={r} phase=publish outcome=x reason=y"] +
             base[race + 3:], "defect", "F", "fail closed"),
            (drop(wt), "defect", "S1", "no wait"),
        ]
        for lines, v, rule, why in ctl:
            expect(lines, v, rule, f"{p}: {why}")
        # the three new controls on the real trace too
        expect(base + [f"D2_RECV_WAITER_RACE_UNWIND tid={r}"], "defect", "O", f"{p}: orphan companion")
        expect(base[:pair + 1] + base[race:pair + 1] + base[pair + 1:], "defect", "S1",
               f"{p}: duplicate publication sharing one recovery")
        cm = next(i for i, x in enumerate(base) if is_commit(x) and
                  f"record_index={rec[0]} record_generation={rec[1]} " in x)
        expect(base[:cm + 1] + [base[cm]] + base[cm + 1:], "defect", "S6", f"{p}: duplicate terminal commit")
    print(f"D2_PUBLISH_RACE_CHECK_SELF_TEST result=ok synthetic={n_syn} fixtures={len(fixtures)} "
          f"cases={count}")


def main(argv):
    if argv and argv[0] == "--self-test":
        self_test(argv[1:])
        return 0
    quiet = bool(argv) and argv[0] == "--quiet"
    paths = argv[1:] if quiet else argv
    if not paths:
        print(__doc__)
        return 2
    worst = 0
    for p in paths:
        rep = check_text(open(p, errors="replace").read())
        v = rep.verdict()
        worst = max(worst, {"ok": 0, "unverified": 2, "defect": 1}[v] if v != "defect" else 3)
        print(summary(rep) + (f" log={p}" if len(paths) > 1 else ""))
        if not quiet:
            for e in rep.events:
                print(f"  race line={e['line']} tid={e.get('receiver')} route={e.get('route')} "
                      f"wait_gen={e.get('wait_gen')} record={e.get('record')} sender={e.get('sender')} "
                      f"recovered={int(bool(e.get('recovered')))}")
        for m in rep.defects:
            print("  DEFECT " + m)
        for m in rep.unverified:
            print("  UNVERIFIED " + m)
    return {0: 0, 2: 2, 3: 1}[worst]


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
