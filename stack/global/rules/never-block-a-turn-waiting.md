---
id: rule.never-block-a-turn-waiting
kind: rule
title: Never block a turn waiting
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
evidence:
- summary: Agents in foreground poll loops could not receive the orchestrator's messages for hours.
  date: 2026-10-20
origin:
  type: own
topics: [process, delegation]
---
**Purpose:** an agent blocked in a wait can't receive messages; agents in poll loops were unreachable for hours.

**Default:** don't wait inside a turn with a foreground sleep or poll loop (on a lock, a job or a review): run the
wait as a background task and end the turn, so its completion wakes you and messages can reach you. Never edit a
script that a running background task executes; write a new file.

**When to depart:** a short, bounded foreground wait (seconds, with a deadline) is fine when nothing else can go on
meanwhile.
