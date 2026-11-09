---
id: rule.orchestrator-cleanup
kind: rule
title: Orchestrators share verified data and clear finished scratch
audience: [all]
roles: [orchestrator]
scope: global
start: role
core:
- line: "Share verified benchmark datasets instead of preparing them again, and clear finished scratch and stale build outputs while reporting reclaimed space and keeping caches planned work reuses. Fix a recurring leak at its producer before removing its remnants."
  roles: [orchestrator]
visibility: public
origin:
  type: own
topics: [cleanup, disk]
status: retired
superseded_by: rule.clean-up-after-use
retired:
  date: 2026-11-09
  reason: "merged into rule.clean-up-after-use (share verified datasets, clear finished scratch and report reclaimed space, fix recurring leaks at the producer) (rules condensation, owner 8-9 Nov, items 10-12)"
---
Share verified benchmark datasets instead of preparing them again. Clear finished scratch and stale build outputs and
report the space reclaimed, keeping caches that planned work reuses. A leak that recurs is fixed at its producer before
its remnants are removed.
