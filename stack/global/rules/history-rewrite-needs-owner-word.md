---
id: rule.history-rewrite-needs-owner-word
kind: rule
title: No rewriting pushed history without the owner's word
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [git]
---
Never rewrite or force-push commits that are already pushed without the owner's explicit word for that repository and
those commits. Unpushed branches may be rewritten before their first push.
