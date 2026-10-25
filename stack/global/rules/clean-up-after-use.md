---
id: rule.clean-up-after-use
kind: rule
title: Clean up owned containers and build outputs after use
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: One job leaked 275 GB of per-process test stores on a shared host; disk overflow recurred several times.
  date: 2026-10-25
origin:
  type: own
topics: [containers, cleanup, disk]
---
Stop containers you own once they are unused, and remove disposable container resources after their final use. Remove
a worktree's build outputs when its phase is done. Keep images, data and caches that planned work will reuse.
