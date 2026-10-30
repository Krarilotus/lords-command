---
id: rule.orchestrator-queue-discipline
kind: rule
title: Orchestrators answer the owner first and work in batches
audience: [all]
roles: [orchestrator]
scope: global
status: active
visibility: public
basis:
- quote: "One thing i would like is, that Orchestrators have the ability to take messages in t3 threads all together instead of going one by one, and that way the queue gets empty every time, and all thigns can be handled and also compounting effects will work beter that way."
  date: 2026-10-30
  by: owner
- quote: "it would both - save tokens - give compounding information because information is less likely to get stale"
  date: 2026-10-30
  by: owner
evidence:
- summary: "Two orchestrators, 30 Oct: read-then-cancel lost nothing only because no item had attachments; a turn that stayed running held six items for two hours; twice a child's final result sat only in its task status, with no notification and no queue item. Promoting a queued item into the running turn was verified the same day."
  date: 2026-10-30
origin:
  type: own
topics: [delegation, process]
---
Answer an owner message in the next turn, before anything queued. Take everything that waits for you at once: at the
start of a turn and again before its final reply, pull every item queued in your own thread into the running turn
(promote it; don't read and cancel, since reading returns only text and a cancel can drop an attachment), so the queue
is empty when the turn ends. A task's automatic completion notice can't be promoted: read it whole, then cancel it. The queue isn't the only inbox: at each checkup also read the result of every child
that finished since the last one, since a result can sit there with no notification; don't poll children that are
still running. Handle what arrived together as one batch: one message per agent,
and close each request with its disposition, not merely because it was read. To a busy agent, read its state first and
send one steer that holds whatever it is doing, never several corrections. Delegate work that would take more than a
few minutes of your own tool calls.
