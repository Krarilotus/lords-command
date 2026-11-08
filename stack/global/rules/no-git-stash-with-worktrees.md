---
id: rule.no-git-stash-with-worktrees
kind: rule
title: No git stash in a repository with several worktrees
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "Boundary: no git stash (push, pop, apply, drop) in a repository with several worktrees, which share one stash list; set a change aside by copying the file or committing it to a throwaway branch."
visibility: public
origin:
  type: own
topics: [git]
---
Never use `git stash` (push, pop, apply, drop) in a repository with several worktrees: they all share one stash list.
To set a change aside, copy the file elsewhere or commit it to a throwaway branch.
