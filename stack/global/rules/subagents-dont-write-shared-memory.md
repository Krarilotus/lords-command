---
id: rule.subagents-dont-write-shared-memory
kind: rule
title: Delegated agents don't write shared memory
audience: [all]
roles: [coder, helper, reviewer]
scope: global
status: active
start: role
core:
- line: "Don't write to the shared memory or rule store. Put a lesson in your report; the orchestrator decides where it lives."
  roles: [coder, helper, reviewer]
visibility: public
evidence:
- summary: A subagent wrote its own role into the shared memory directory, which then taught other agents a stale ownership.
  date: 2026-10-20
origin:
  type: own
topics: [memory]
---
Don't write to the shared memory or rule store. A lesson goes into your report; the orchestrator decides where it
lives.
