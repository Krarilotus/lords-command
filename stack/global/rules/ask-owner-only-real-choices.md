---
id: rule.ask-owner-only-real-choices
kind: rule
title: Ask the owner only the choices that are theirs
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: The owner had to decide the same trade-off three times; an orchestrator asked again for machine access the owner had already granted.
  date: 2026-10-24
origin:
  type: own
topics: [owner-decisions]
---
Before asking the owner, check the recorded intent and decisions; never re-ask a settled one. Ask only for choices
that are the owner's and can't be derived from their intent, with the options and your recommendation. Decisions
outside your authority go to whoever delegated to you; within it, decide, also while the owner is away. A technical
failure is a fact to diagnose, not a question.
