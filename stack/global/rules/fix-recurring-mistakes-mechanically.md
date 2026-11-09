---
id: rule.fix-recurring-mistakes-mechanically
kind: rule
title: Fix a recurring mistake at the strongest level
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
evidence:
- summary: "(from rule.one-tool-per-stage, merged 9 Nov) Per-agent pipeline scripts, private launchers and duplicate deploy tools each re-solved the same stage and diverged."
  date: 2026-10-25
- summary: "8-9 Nov: review blocks kept recurring while fixes chased call sites: one package's sibling sweep found 159 sites of the same shape, alias slices were blocked on four wrong answers of one shape, and an export fix was blocked twice. Each was steered to one type or contract at the owner (master state, 8 Nov 22:25-9 Nov 00:03)."
  date: 2026-11-09
origin:
  type: own
topics: [process, design]
---
**Purpose:** a mistake fixed where it shows up comes back at the next caller; fixed at its owner, it can't.

**Default:** fix a recurring mistake, or a class of bug, at the strongest level available: architecture (it can't
happen), then types (it doesn't compile), then a checked file or lint, then a test, and prose last. Put the fix in the
module that owns the invariant, so every caller gets it; when you hit one, propose its mechanical form. A fix at a call
site states why the owner can't hold the invariant.

Each stage of a change (build and land, time, prove, run elsewhere, review) has one project tool that owns its rules.
Use it; don't write a private pipeline, launcher, pass stamp or policy workaround beside it. A gap in the tool goes to
its owner as a defect. (Merged 9 Nov from rule.one-tool-per-stage.)

**When to depart:** a smaller remedy is right when the strong form costs more than the mistake it prevents; say which
and why. Never bypass a check or a policy to do it.
