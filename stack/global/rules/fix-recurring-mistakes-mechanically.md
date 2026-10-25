---
id: rule.fix-recurring-mistakes-mechanically
kind: rule
title: Fix a recurring mistake at the strongest level
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [process, design]
---
Fix a recurring mistake at the strongest level available: architecture (it can't happen), then types (it doesn't
compile), then a checked file or lint, then a test, and prose last. When you hit one, propose its mechanical form.
