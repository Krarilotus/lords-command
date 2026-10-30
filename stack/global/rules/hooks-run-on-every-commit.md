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
- summary: A project's shared checkout forbids resets ("nothing is stashed or reset"), so the recovery defers to the repo's history policy (consistency audit C02).
  date: 2026-10-30
origin:
  type: own
topics: [git, commits]
---
Commit through the repository's normal hooks: never skip them and never repoint the hooks path. Never force-stop a
running hook, because git then commits unhooked. Stop a committer only between commits, then compare the last commit
with its log. Reset any commit that went in unhooked where the repo's history policy allows a reset; where it doesn't
(a shared checkout, a pushed commit), revert it with a new commit, commit the change again through the hooks, and tell
the repo's owner.
