---
id: rule.one-tool-per-stage
kind: rule
title: One tool per stage of a change
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
start: core
core: "Use the project's owning tool for each change stage. Do not write a private pipeline, launcher, pass stamp or policy workaround beside it."
visibility: public
evidence:
- summary: Per-agent pipeline scripts, private launchers and duplicate deploy tools each re-solved the same stage and diverged.
  date: 2026-10-20
origin:
  type: own
topics: [process]
status: retired
superseded_by: rule.fix-recurring-mistakes-mechanically
retired:
  date: 2026-11-09
  reason: "merged word for word into rule.fix-recurring-mistakes-mechanically, with its evidence (rules condensation, owner 8-9 Nov, items 10-12)"
---
Each stage of a change (build and land, time, prove, run elsewhere, review) has one project tool that owns its rules.
Use it; don't write a private pipeline, launcher, pass stamp or policy workaround beside it.
