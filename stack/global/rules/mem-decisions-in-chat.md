---
id: rule.mem-decisions-in-chat
kind: rule
title: Bring decisions to the owner in chat, as self-contained numbered items
audience: [all]
roles: [orchestrator]
scope: global
status: active
start: role
core:
- line: "To make the owner's decisions easy: bring them in chat as self-contained numbered items (why, options, recommendation), with no forward references and no file to read; fold interdependent choices into one proposal with recommendations as defaults, asking only what changes its direction; when gathering design intent, ask one brief question per turn and keep the rest in the open-questions record."
  roles: [orchestrator]
visibility: public
basis:
- quote: "I want to only decide line by line offered to me through you in chat … I can't be bothered to check different documents all over the place, that's for your organization with the subagents."
  date: 2026-10-03
  by: owner
  source: "recorded only in this entry; not found in his intent files (corpus audit 8 Nov, U1)"
- quote: "please not too many forward references its hard to follow then"
  date: 2026-10-21
  by: owner
  source: "recorded only in this entry; not found in his intent files (corpus audit 8 Nov, U1)"
- quote: "I think you should fold al lthe thigns into a bigger proposal i can sign off with taking your recommendations as defaults and ask only questions for the overall proposal so it is easier for me to grasp everything how it works together"
  date: 2026-10-30
  by: owner
  source: "recorded only in this entry; not found in his intent files (corpus audit 8 Nov, U1)"
evidence:
- summary: "Example item: '1. Delete the build cache (frees 268 GB; risk: other apps rebuild slower) → yes/no'"
  date: 2026-10-03
- summary: "A storage design reached the owner as six separate technical choices; he asked for one integrated proposal with the recommendations as defaults instead"
  date: 2026-10-30
- summary: "(merged 9 Nov from a private rule on one question per turn) In design interviews the owner couldn't answer many questions at once, and one answer often settled others (18 Oct)."
  date: 2026-10-18
origin:
  type: own
topics: [owner-decisions, reporting, process]
links: [rule.ask-owner-only-real-choices]
---
**Purpose:** the owner decides from the chat, often on another device; anything he must assemble himself costs him
time and gets decided worse.

**Default:** present decisions and approvals directly in the chat, as short numbered items the owner can answer one by
one, each explained where it stands (what it is, why it is a decision, the options and a recommendation), without
forward references. Don't ask the owner to read a file to decide; extract the decision points from plans and reviews
yourself and link files only as optional background. When a design's decisions depend on each other, fold them into
one proposal: explain how the parts work together, take the recommendations as defaults the owner signs off as a
whole, and ask only the questions that change the overall direction.

When gathering design intent from the owner, ask one question per turn, the one that matters most, briefly, with
options and a recommendation. After an answer, check which open questions it settled; keep the rest in the project's
open-questions record, not in the reply.

**When to depart:** group or order items differently when that makes them easier to answer, and say so in a line.
