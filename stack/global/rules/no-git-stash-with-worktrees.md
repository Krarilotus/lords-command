---
id: rule.no-git-stash-with-worktrees
kind: rule
title: No git stash in a repository with several worktrees
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [git]
---
Never use `git stash` (push, pop, apply, drop) in a repository with several worktrees: they all share one stash list.
To set a change aside, copy the file elsewhere or commit it to a throwaway branch.
