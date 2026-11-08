---
id: rule.clean-up-after-use
kind: rule
title: Clean up owned containers and build outputs after use
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "To keep shared disks and hosts usable: stop your containers once unused, remove your disposable resources after last use and a worktree's build outputs when its phase ends, and keep images, data and caches planned work reuses. Share verified datasets instead of preparing them again; fix a recurring leak at its producer."
visibility: public
evidence:
- summary: One job leaked 275 GB of per-process test stores on a shared host; disk overflow recurred several times.
  date: 2026-10-25
origin:
  type: own
topics: [containers, cleanup, disk]
---
**Purpose:** leaks fill shared disks and hosts and stop everyone's work; rebuilding what exists wastes hours.

**Default:** stop containers you own once they are unused, and remove disposable container resources after their
final use. Remove a worktree's build outputs when its phase is done. Keep images, data and caches that planned work
will reuse. Share verified benchmark datasets instead of preparing them again. An orchestrator also clears finished
scratch and stale build outputs and says how much space came back. A leak that recurs is fixed at its producer
before its remnants are removed. What you may delete is bounded by rule.delete-only-what-you-created.

**When to depart:** keep something past its phase when planned work will reuse it, and say what and until when.
