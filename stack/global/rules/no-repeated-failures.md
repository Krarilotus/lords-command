---
id: rule.no-repeated-failures
kind: rule
title: No repeated failures; cause first
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
basis:
- quote: 'not running the same tasks and gates in loops that failed previously'
  date: 2026-10-20
  by: owner
evidence:
- summary: Most failed scheduler jobs over a week showed no recorded cause and many were resubmitted unchanged; 22 % of host-hours went to failed or cancelled jobs.
  date: 2026-10-25
origin:
  type: own
topics: [debugging, process]
---
Something that failed (a job, gate, proof, hook or review) is not run again until you have kept its raw output, named
its cause and fixed that cause, without generalising beyond it. One small, bounded repair within your own task is fine.
A second failure for the same cause stops the retries: hand it, with the evidence, to whoever owns the cause. Never
weaken a guard, skip a check or invent evidence to keep work moving.
