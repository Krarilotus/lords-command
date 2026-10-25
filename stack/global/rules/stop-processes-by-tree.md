---
id: rule.stop-processes-by-tree
kind: rule
title: Stop processes by their own process tree
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: Stopping a background shell task left its looping script running; a live script was also declared dead because the shell's process list shows no arguments.
  date: 2026-10-20
origin:
  type: own
topics: [process, debugging]
---
Stop a process by its own PID and process tree, never by matching command-line text. After stopping a background
script that loops, submits or commits, check that none of its children survived and that nothing it released started
its next step.
