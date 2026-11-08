---
id: rule.no-repeated-failures
kind: rule
title: No repeated failures; cause first
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "To stop blind retries: before rerunning a failed job, gate, proof, hook or review, keep its raw output and name and fix its cause, without generalising beyond it (a small bounded repair, or a diagnostic rerun that says what it tests, is fine). A second same-cause failure goes with evidence to the cause's owner. Never weaken a guard, skip a check or invent evidence."
visibility: public
basis:
- quote: 'not running the same tasks and gates in loops that failed previously'
  date: 2026-10-20
  by: owner
  source: "recorded only in this entry; not found in his intent files (corpus audit 8 Nov, U1)"
evidence:
- summary: Most failed scheduler jobs over a week showed no recorded cause and many were resubmitted unchanged; 22 % of host-hours went to failed or cancelled jobs.
  date: 2026-10-25
origin:
  type: own
topics: [debugging, process]
links: [skill.stop-the-line]
---
**Purpose:** unchanged resubmissions burned 22 % of host-hours in a week; a failure rerun without a cause teaches
nothing.

**Default:** don't rerun what failed (a job, gate, proof, hook or review) until you have kept its raw output, named its
cause and fixed that cause, without generalising beyond it; one small, bounded repair within your own task is fine. A
second failure for the same cause stops the retries: hand it, with the evidence, to the cause's owner. The diagnosis
method is skill.stop-the-line.

**Never** weaken a guard, skip a check or invent evidence to keep work moving: that holds without exception.

**When to depart:** a bounded rerun that reproduces or isolates the cause is diagnosis, not a retry; say what it
tests. A failure the owner of the host or guard has already explained (a guard kill, a host fault) is rerun as that
owner's rule says.
