import json, sys, statistics
Q = sys.argv[1]
ports = ["x86_64", "aarch64", "riscv64"]
def fmt(xs): return " ".join(str(x) for x in xs)
print("| port | profile | arm | clean rounds | direct wakes | requeued | NR 17 calls | mutex re-parks (actually parked) | mutex `ValueChanged` | worker unlock wake calls / woke | broadcaster unlock calls / woke | elapsed median (min–max) |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|")
rb = []
for p in ports:
    for prof in ["gp", "rtb"]:
        d = json.load(open(f"{Q}/W_seal_{prof}_{p}/summary.json"))
        for arm in ["rq", "wa"]:
            rs = sorted(d["pb"]["rounds"][arm], key=lambda r: r["round"])
            cls = {}
            for r in rs: cls[r["class"]] = cls.get(r["class"], 0) + 1
            el = [r["elapsed_ns"] / 1e6 for r in rs]
            nr17 = [1 if r["requeued"] or arm == "rq" else 0 for r in rs]
            print(f"| {p} | {prof} | {arm} | {cls.get('clean',0)}/{len(rs)}"
                  + ("" if len(cls) == 1 else f" ({cls})")
                  + f" | {fmt(r['direct_woken'] for r in rs)} | {fmt(r['requeued'] for r in rs)}"
                  + f" | {'1 each' if arm == 'rq' else '0'}"
                  + f" | {fmt(r['mutex_reparks'] for r in rs)} | {fmt(r['mutex_value_changed'] for r in rs)}"
                  + f" | {fmt(str(r['worker_unlock_wake_calls'])+'/'+str(r['worker_unlock_woke']) for r in rs)}"
                  + f" | {fmt(str(r['broadcaster_unlock_wake_calls'])+'/'+str(r['broadcaster_unlock_woke']) for r in rs)}"
                  + f" | {statistics.median(el):.0f} ms ({min(el):.0f}–{max(el):.0f}) |")
        b = d["rb"]
        if prof == "rtb":
            rb.append((p, b))
print()
print("| port | induced drains | park tick (CPU) | release instant | rejoin tick | rejoin lateness (ticks) | deadline → settled (CLOCK_MONOTONIC) |")
print("|---|---|---|---|---|---|---|")
for p, b in rb:
    print(f"| {p} | {b['induced_exhaustion']} | {b['park_tick']} (CPU {b['park_cpu']}) | {b['release']} | {b['rejoin_tick']} | {b['release_lateness_ticks']} | {(b['settled_ns']-b['deadline_ns'])/1e6:.1f} ms after |")
for p in ports:
    d = json.load(open(f"{Q}/W_seal_gp_{p}/summary.json"))
    print(f"<!-- gp rb {p}: {json.dumps(d['rb'])} -->")
for p in ports:
    for prof in ["gp","rtb"]:
        d = json.load(open(f"{Q}/W_seal_{prof}_{p}/summary.json"))
        print(f"<!-- lh {p} {prof}: {json.dumps(d['lh'])} -->")
