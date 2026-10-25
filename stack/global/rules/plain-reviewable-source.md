---
id: rule.plain-reviewable-source
kind: rule
title: Code lives only as plain, reviewable source in its owning place
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
basis:
- quote: "Everything has its place and needs to make semantic sense."
  date: 2026-10-25
  by: owner
- quote: "not just tooling to get it running but sorting of the meta information is crucial and actually making sure it's reviewable"
  date: 2026-10-25
  by: owner
evidence:
- summary: A benchmark harness carried four source files as one packed, compressed string and wrote them into the build at run time; the code was invisible to review and code search, and its untracked temp stores filled a shared machine's disk
  date: 2026-10-25
origin:
  type: own
topics: [review, design, check-candidate]
---
Code exists only as plain, reviewable source in the place that owns it. No encoded, packed or generated-at-run-time
code, and no build-time patching of source: what is built and measured is exactly what is in the repo.
