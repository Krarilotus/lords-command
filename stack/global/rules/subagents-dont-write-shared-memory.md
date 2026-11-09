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
- line: "Boundary: don't write to the shared memory or rule store; a lesson goes in your report and the orchestrator decides where it lives. Only an explicit owner decision that reached you directly is yours to record, verbatim."
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

The one exception is the receiver's duty in the owner table: an explicit owner decision that reaches you directly is
recorded by you, verbatim with quote, date and source, through the store's hooks. Recording doesn't include
interpreting it or adding a lesson of your own; those still go in your report. (Resolves the clash the 8 Nov corpus
audit found between this entry and the owner table.)
