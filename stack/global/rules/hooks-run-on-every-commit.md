---
id: rule.hooks-run-on-every-commit
kind: rule
title: Every commit runs its hooks; never stop a running hook
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: Force-stopped committers let git commit without its hooks; stopping only between commits and comparing the last commit with the committer's log caught them.
  date: 2026-10-20
origin:
  type: own
topics: [git, commits]
---
Commit through the repository's normal hooks: never skip them and never repoint the hooks path. Never force-stop a
running hook, because git then commits unhooked. Stop a committer only between commits, then compare the last commit
with its log and reset any commit that went in unhooked.
