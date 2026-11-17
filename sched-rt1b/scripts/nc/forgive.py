# RT-SCHED1B negative control: replenishment forgives the whole debt (the rule
# SCHED-CONTRACT1-CORRECTION removed). Run from the candidate worktree root.
p = 'src/kernel/sched_budget.rs'
s = open(p).read()
a = '''        let before_last = ctx.debt - ctx.debt.min((n - 1).saturating_mul(budget));
        let paid_last = budget.min(before_last);
        ctx.remaining = budget - paid_last;
        ctx.debt = before_last - paid_last;'''
assert s.count(a) == 1
open(p, 'w').write(s.replace(a, '''        ctx.remaining = budget.saturating_sub(ctx.debt);
        ctx.debt = 0;'''))
