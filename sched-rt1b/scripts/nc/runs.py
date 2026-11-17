# RT-SCHED1B negative control: an exhausted context is never ineligible (it keeps running and
# keeps its queue position). Run from the candidate worktree root.
p = 'src/kernel/scheduler.rs'
s = open(p).read()
a = 'self.budget.enabled && self.budget.get(tid.0).is_some_and(|c| c.exhausted())'
assert s.count(a) == 1
open(p, 'w').write(s.replace(a, 'false && self.budget.get(tid.0).is_some_and(|c| c.exhausted())'))
