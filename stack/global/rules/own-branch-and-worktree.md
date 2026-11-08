---
id: rule.own-branch-and-worktree
kind: rule
title: Work in your own branch and worktree
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "Boundary: never modify another agent's worktree or branch, and never reset a shared checkout. Make your changes in your own branch and worktree; reading needs none."
visibility: public
origin:
  type: own
topics: [git]
---
Never modify another agent's worktree or branch, and never reset a shared checkout: these are boundaries.

Changes are made in your own branch and worktree, so nobody else's work moves under them. Read-only work (reading
code, a review in a detached disposable worktree, a status query) needs no worktree of its own.
