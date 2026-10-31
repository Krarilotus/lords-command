---
id: rule.reports-short-and-plain
kind: rule
title: A report is a decision-ready handoff, backed by a durable record
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
basis:
- quote: "I don'T see why we put an arbitrary number on it, that came out of nowhere an should be raised as an issue, there must be a better design there out there that already covers that"
  date: 2026-10-26
  by: owner
- quote: "So yes briefs get shorter and we have some overall duties we can also redesign, and overrule with our project specific stuff"
  date: 2026-10-26
  by: owner
evidence:
- summary: Agent reports grew to 250-440 KB per note in dense prose; activity volume stood in for delivered work.
  date: 2026-10-25
- summary: "Research on agent and human handoffs (26 Oct): no reviewed source supports a fixed word count; the documented practice is a compact, ordered return (outcome first, decisions, risks) with the full detail in a durable artifact read in sections, and batch results consolidated before acting."
  date: 2026-10-26
  links: ["https://www.anthropic.com/engineering/multi-agent-research-system", "https://sre.google/sre-book/managing-incidents/", "https://www.ahrq.gov/teamstepps-program/curriculum/communication/tools/sbar.html"]
origin:
  type: own
topics: [reporting, delegation]
links: [rule.briefs-point-not-restate]
---
Report a result, a blocker or a decision request, not progress, in plain words. Lead with the outcome and what it
means, then what the recipient needs for the next step: the decisions needed (or "none"), each with a recommendation
and its owner; the result or change, with commits or finding ids; the verification, with what passed, failed or
wasn't run; and the open risks and unknowns. Blockers, uncertainty and material findings stay in the report; they are
never cut to make it shorter. Length follows the decision, not a word count.

Full evidence (findings, reasoning, reproductions, logs) goes into one durable record in the **evidence home**, a named
place each project sets, with stable ids and an index at the top, linked from the report and opened when needed. The
evidence home is `Harness/notes`, a private repository that syncs itself. Don't repeat an automatic completion notice
or narrate routine activity.

Every report on code or design also gives its owner and level verdict, and one line on whether you would build it
this way again (the level-and-cost skill).

Machine-readable fields only where a consumer checks them; reuse the harness's own result and check data before adding
any. A declared batch of results is collected and then consolidated top-down, by requirement and owner, before
anything is assigned. A project may make a justified exception: it names this rule, the difference, its reason, the
approving owner and when it is reviewed, and refers to this rule instead of copying it.

```text
Outcome:  status, verdict, what it means; the revision it applies to
Decision: none, or id + recommendation + owner + what it blocks
Result:   the change or the finding ids; commits
Evidence: checks and their scope; links; what wasn't run
Open:     risks, uncertainty, provisional choices, or none
Detail:   the record and section; complete, or the gap
```
