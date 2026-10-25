---
id: rule.delete-only-what-you-created
kind: rule
title: Delete only what you created, by exact path
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [cleanup, disk]
---
Delete only what you created, by exact path, never by a glob in a shared directory, unless the owner authorises a wider
cleanup. Nothing process-wide from library code: resources stay scoped to their owner.
