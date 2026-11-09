---
id: rule.no-git-stash-with-worktrees
kind: rule
title: No git stash in a repository with several worktrees
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "Boundary: never change another worktree's work through the repository's shared stash list. Default: in a repository with several worktrees, set a change aside by copying the file or committing it to a throwaway branch, not with git stash."
visibility: public
origin:
  type: own
topics: [git]
---
**Boundary:** all worktrees of a repository share one stash list, so a stash push, pop, apply or drop can change or
lose another agent's work. Never change another worktree's work that way.

**Default:** in a repository with several worktrees, don't use `git stash` (push, pop, apply, drop): to set a change
aside, copy the file elsewhere or commit it to a throwaway branch.

**When to depart:** a stash use that provably touches only your own entry (applying a recorded stash by its object id
in your own isolated worktree, say) is fine; say which and why.
