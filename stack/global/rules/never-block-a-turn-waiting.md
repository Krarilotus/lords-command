---
id: rule.never-block-a-turn-waiting
kind: rule
title: Never block a turn waiting
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: Agents in foreground poll loops could not receive the orchestrator's messages for hours.
  date: 2026-10-20
origin:
  type: own
topics: [process, delegation]
---
Never wait inside a turn with a foreground sleep or poll loop (on a lock, a job or a review): run the wait as a
background task and end the turn, so its completion wakes you and messages can reach you. Never edit a script that a
running background task executes; write a new file.
