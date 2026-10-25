---
id: rule.mem-less-is-more-ui
kind: rule
title: Less is more, above all in interface text
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
basis:
- quote: "the UI must speak for itself — a slick version"
  date: 2026-10-21
  by: owner
evidence:
- summary: Three design rounds failed partly because agents produced large amounts of explanatory UI text; the owner had said it many times
  date: 2026-10-05
- summary: The owner rejected visible explanatory text in a panel's lines; all text moved to tooltips
  date: 2026-10-21
origin:
  type: own
topics: [design]
---
Every word on screen must help the user do what they came for: no explanatory paragraphs, no labels describing the
system, no eyebrow labels, slogans or rhetorical questions, no repeated text. When in doubt, cut it. Extend existing
views rather than redesigning for decoration, and check outputs against this before showing them.
