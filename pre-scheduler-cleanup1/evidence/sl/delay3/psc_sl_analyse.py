# For each SCHED-LOCK1 delay run: every hold, the observer's SEC records just before HOLD_BEGIN
# (the pre-hold boundary the scratch delay targets), the observer's own SECs inside the hold, and
# the producer's HOLD_END commit count (f1 = B_COMMITS). Agreement = B_COMMITS == in-hold SECs.
import re,sys,os
d=sys.argv[1]
for run in sorted(os.listdir(d)):
    p=os.path.join(d,run,'boot.norm.log')
    if not os.path.isfile(p): continue
    recs={}
    for l in open(p,errors='replace'):
        m=re.match(r'SL1W_R seq=(\d+) kind=(\w+) cpu=(\d+) exec=(\d+) f0=(\d+) f1=(\d+)',l)
        if m: recs.setdefault(int(m.group(1)),m.groups())
    seqs=sorted(recs); out=[]; disagree=0
    for s in seqs:
        if recs[s][1]!='HOLD_BEGIN': continue
        obs=int(recs[s][5])
        pre=[t for t in range(s-3,s) if t in recs and recs[t][1]=='SEC' and int(recs[t][2])==obs]
        end=next(t for t in seqs if t>s and recs[t][1]=='HOLD_END')
        inhold=[t for t in seqs if s<t<end and recs[t][1]=='SEC' and int(recs[t][2])==obs and int(recs[t][3])==obs]
        bc=int(recs[end][5]); disagree+= bc!=len(inhold)
        out.append(f"hold@{s} obs={obs} pre_hold_obs_SEC={pre} in_hold_SEC={len(inhold)} B_COMMITS={bc}")
    print(f"{run} disagreements={disagree} | "+'; '.join(out))
