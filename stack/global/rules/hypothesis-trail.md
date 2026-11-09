---
id: rule.hypothesis-trail
kind: rule
title: "Investigation trail"
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
origin:
  type: own
topics: [debugging, notes]
---
**Purpose:** without a trail the same dead end gets tried again, by you or the next agent.

**Default:** an investigation of more than one try keeps a trail, one line per hypothesis: the thought, the test, the
result and its status. Disproven ones stay. Read it before the next try; when something is hard, record the next angle
and keep going.

**When to depart:** where the issue, the review or the hand-over note already holds that trail, keep it there instead
of a second copy.
