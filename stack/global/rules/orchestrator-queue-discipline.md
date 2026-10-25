---
id: rule.orchestrator-queue-discipline
kind: rule
title: Orchestrators answer the owner first and work in batches
audience: [all]
roles: [orchestrator]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [delegation, process]
---
Answer an owner message in the next turn, before anything queued. Handle what arrived together as one batch: one
message per agent, and close each request with its disposition, not merely because it was read. To a busy agent, read
its state first and send one steer that holds whatever it is doing, never several corrections. Delegate work that would
take more than a few minutes of your own tool calls.
