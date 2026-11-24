S=/tmp/claude-0/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043/scratchpad; python3 - <<EOF
import json
found=[]
with open("/root/.claude/projects/-home-user-YARM/e3c47cf0-be59-5ee6-a6fa-73064b3ba043.jsonl") as fh:
    for line in fh:
        try: o=json.loads(line)
        except Exception: continue
        msg=o.get("message") or {}
        content=msg.get("content")
        if not isinstance(content,list): continue
        for c in content:
            if c.get("type")=="tool_use" and c.get("name")=="Bash":
                cmd=c.get("input",{}).get("command","")
                if "new_head='''//! Reusable VFS client helpers" in cmd:
                    found.append(("head",cmd))
                if "tests='''    // ── VFS-CORRECT1A: the transaction through a scripted transport" in cmd:
                    found.append(("tests",cmd))
for i,(k,cmd) in enumerate(found):
    open(f"$S/vc1a-dev/recover_{i}_{k}.sh","w").write(cmd)
    print(i,k,len(cmd))
EOF