import re,sys,os,collections
openers=[".with_cpu_sched_split(",".with_cpu_sched_and_tcbs_split(",".with_cpu_admission_split(",".with_all_cpus_sched_split(",
         ".lock_cpu(",".lock_cpu_admission(",".lock_all()",".with_scheduler_state(",".with_scheduler_state_mut(",".scheduler_state()"]
def prod(path,s):
    i=s.find("\n#[cfg(test)]\nmod ")
    return s if i<0 else s[:i]
rows=[]
for root,_,fs in os.walk("src"):
    for f in fs:
        if not f.endswith(".rs") or "tests" in f or "_tests" in root: continue
        p=os.path.join(root,f); s=prod(p,open(p).read())
        for op in openers:
            for m in re.finditer(re.escape(op),s):
                line=s.count("\n",0,m.start())+1
                pre=s[s.rfind("\n",0,m.start())+1:m.start()]
                if pre.strip().startswith("//") or "fn " in pre: continue
                # body: for closure openers, matched parens; for lock_* take rest of enclosing block (approx 60 lines)
                body=s[m.end():]
                if op.endswith("("):
                    d=1;e=0
                    for i,ch in enumerate(body):
                        if ch=='(':d+=1
                        elif ch==')':
                            d-=1
                            if d==0:e=i;break
                    body=body[:e]
                else:
                    # guard-scoped: until end of enclosing block
                    d=0;e=len(body)
                    for i,ch in enumerate(body):
                        if ch=='{':d+=1
                        elif ch=='}':
                            if d==0:e=i;break
                            d-=1
                    body=body[:e]
                fn=None
                for fm in re.finditer(r"fn (\w+)",s[:m.start()]): fn=fm.group(1)
                flags=[]
                if "yarm_log!" in body: flags.append("LOG")
                if re.search(r"tcbs|with_task_tcbs|with_tcbs",body): flags.append("RANK2")
                if re.search(r"for_each_queued|dispatch_next_accepted|accepted_selection|linear_contains|iter\(\)\.any|placement_of|tid_present_anywhere|count_tid|remove_tid|withdraw",body): flags.append("SCAN")
                rows.append((p,line,fn,op,",".join(flags),len(body.splitlines())))
c=collections.Counter()
for r in rows: print("%s:%d fn=%s op=%s flags=[%s] lines=%d"%r)
print("TOTAL",len(rows))
for r in rows:
    for fl in r[4].split(","):
        if fl: c[fl]+=1
print(c)
