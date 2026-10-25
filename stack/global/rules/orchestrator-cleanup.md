---
id: rule.orchestrator-cleanup
kind: rule
title: Orchestrators share verified data and clear finished scratch
audience: [all]
roles: [orchestrator]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [cleanup, disk]
---
Share verified benchmark datasets instead of preparing them again. Clear finished scratch and stale build outputs and
report the space reclaimed, keeping caches that planned work reuses. A leak that recurs is fixed at its producer before
its remnants are removed.
