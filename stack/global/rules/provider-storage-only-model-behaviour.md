---
id: rule.provider-storage-only-model-behaviour
kind: rule
title: Provider storage holds only necessary model-specific behaviour
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
origin:
  type: own
topics: [memory, tools]
links: [intent.organisation-principles]
---
Lords Command is the unified base for all agents, whatever the provider; only behaviour
that is specific to one model and needed to adjust it there lives in that provider's own storage.
