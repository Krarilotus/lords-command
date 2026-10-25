---
id: rule.one-tool-per-stage
kind: rule
title: One tool per stage of a change
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: Per-agent pipeline scripts, private launchers and duplicate deploy tools each re-solved the same stage and diverged.
  date: 2026-10-20
origin:
  type: own
topics: [process]
---
Each stage of a change (build and land, time, prove, run elsewhere, review) has one project tool that owns its rules.
Use it; don't write a private pipeline, launcher, pass stamp or policy workaround beside it.
