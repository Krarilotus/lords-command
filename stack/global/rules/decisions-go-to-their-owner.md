---
id: rule.decisions-go-to-their-owner
kind: rule
title: "Decision routing"
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
basis:
- quote: "They shoudl target the correct owners of correct decisions, if they have to say sth about thigns they can and interfere or even check up on thigns and other threads themselves."
  date: 2026-10-30
  by: owner
  source: "recorded in the rule corpus, not reproduced in his intent register; its original source is not independently verified (corpus audit 8 Nov, U1)"
- quote: "It should never happen that a message gets relayed 2-3 times between the owners over other relay owners all the time, that just makes thigns HARd and conflicting and information is lost"
  date: 2026-10-30
  by: owner
  source: "recorded in the rule corpus, not reproduced in his intent register; its original source is not independently verified (corpus audit 8 Nov, U1)"
- quote: "don't play relay all the time let the agent talk itself to the fleet owner if needed"
  date: 2026-10-30
  by: owner
  source: "recorded in the rule corpus, not reproduced in his intent register; its original source is not independently verified (corpus audit 8 Nov, U1)"
evidence:
- summary: "Questions for a tool's owner went through a third agent, which cost turns and lost detail. Once, a correction arrived before the message it corrected."
  date: 2026-10-30
origin:
  type: own
topics: [delegation, owner-decisions, process]
links: [rule.ask-owner-only-real-choices]
---
**Purpose:** every relay costs turns and drops detail; a correction once arrived before the message it corrected.

**Default:** send a decision, question or finding straight to the one who owns it, in its own thread, and take the answer from it
directly. Never relay between two owners, and don't ask a third agent to pass something on. If you don't know the
owner, the owner table names it; if nothing does, ask your lead who the owner is, not what the answer is. A choice
inside your lead's task goes to your lead. One that belongs to another owner (a shared tool, a host, another project)
goes to that owner. Your result still goes to whoever gave you the task, with the answer in it.

For shared tools, follow the project's tool-operation and deficiency route before contacting an owner.

You may read other threads and records to check on work that touches yours, and speak up to their owner, with
evidence, where it does. Reading and speaking up don't move the decision: its owner makes it.

**When to depart:** when the owner can't be reached in time, or several owners' choices only make sense together,
coordinate through your lead or the orchestrator, and say why the route differs.
