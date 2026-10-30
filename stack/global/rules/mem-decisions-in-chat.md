---
id: rule.mem-decisions-in-chat
kind: rule
title: Bring decisions to the owner in chat, as self-contained numbered items
audience: [all]
roles: [orchestrator]
scope: global
status: active
visibility: public
basis:
- quote: "I want to only decide line by line offered to me through you in chat … I can't be bothered to check different documents all over the place, that's for your organization with the subagents."
  date: 2026-10-03
  by: owner
- quote: "please not too many forward references its hard to follow then"
  date: 2026-10-21
  by: owner
- quote: "I think you should fold al lthe thigns into a bigger proposal i can sign off with taking your recommendations as defaults and ask only questions for the overall proposal so it is easier for me to grasp everything how it works together"
  date: 2026-10-30
  by: owner
evidence:
- summary: "Example item: '1. Delete the build cache (frees 268 GB; risk: other apps rebuild slower) → yes/no'"
  date: 2026-10-03
- summary: "A storage design reached the owner as six separate technical choices; he asked for one integrated proposal with the recommendations as defaults instead"
  date: 2026-10-30
origin:
  type: own
topics: [owner-decisions, reporting]
links: [rule.ask-owner-only-real-choices]
---
Present decisions and approvals directly in the chat, as short numbered items the owner can answer one by one, each
explained where it stands (what it is, why it is a decision, the options and a recommendation), without forward
references. Never ask the owner to read a file to decide; extract the decision points from plans and reviews yourself
and link files only as optional background. When a design's decisions depend on each other, fold them into one
proposal: explain how the parts work together, take the recommendations as defaults the owner signs off as a whole,
and ask only the questions that change the overall direction.
