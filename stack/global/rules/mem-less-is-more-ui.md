---
id: rule.mem-less-is-more-ui
kind: rule
title: "UI text"
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
basis:
- quote: "the UI must speak for itself — a slick version"
  date: 2026-10-21
  by: owner
  source: "recorded in the rule corpus, not reproduced in his intent register; its original source is not independently verified (corpus audit 8 Nov, U1)"
evidence:
- summary: Three design rounds failed partly because agents produced large amounts of explanatory UI text; the owner had said it many times
  date: 2026-10-05
- summary: The owner rejected visible explanatory text in a panel's lines; all text moved to tooltips
  date: 2026-10-21
origin:
  type: own
topics: [design]
---
**Purpose:** the UI must speak for itself (owner, 21 Oct); explanatory text sank three design rounds.

**Default:** every word on screen must help the user do what they came for: no explanatory paragraphs, no labels
describing the system, no eyebrow labels, slogans or rhetorical questions, no repeated text. When in doubt, cut it;
detail can live in a tooltip. Extend existing views rather than redesigning for decoration, and check outputs against
this before showing them.

**When to depart:** an error message, a label a screen reader needs, or text without which the user can't do the task
stays; say in review why it is needed.
