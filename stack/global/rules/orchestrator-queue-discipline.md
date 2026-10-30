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
- quote: "and it seems the rules for taking in the whole queue not one message at a time when you decide to end your turn and take the next subagent has still not managed to penetrate and actually land, make sure to add that proper"
  date: 2026-10-30
  by: owner
  source: about 23:40, to another orchestrator
evidence:
- summary: "Two orchestrators, 30 Oct: read-then-cancel lost nothing only because no item had attachments; a turn that stayed running held six items for two hours; twice a child's final result sat only in its task status, with no notification and no queue item. Promoting a queued item into the running turn was verified the same day."
  date: 2026-10-30
- summary: "Later that night an orchestrator promoted items in parallel and acted between promotions: each parallel call after the first was interrupted, items stayed queued, and the owner saw them handled one by one."
  date: 2026-10-30
- summary: "Promoting every item still failed in practice: each promotion injects its one message into the running turn and interrupts the calls beside it, so items kept arriving one at a time (an orchestrator, every batch that night). Reading an item returns its full text; only an attachment needs a promotion. Twice an orchestrator said it had taken everything while an item sat queued, because nothing showed it the queue."
  date: 2026-10-30
- summary: "Read-then-cancel reports '[cancelled] Child task ended with status cancelled' to the sender of a message the receiver read and answered; a sender first took two such messages for lost. T3 has no 'taken' state yet (an upstream proposal, with a queue listing)."
  date: 2026-10-31
origin:
  type: own
topics: [delegation, process]
---
Answer an owner message in the next turn, before anything queued. Take everything that waits for you at once, at the
start of a turn and again before its final reply, so the queue is empty when the turn ends: list your own thread's
queue (`t3_queue list`), read every truncated item in full, cancel each text-only item once its text is in hand, and
promote only an item that carries an attachment (a `t3-context://` marker), since a cancel would drop it. Promote one
at a time with no other call beside it (a promotion interrupts calls in flight). Then answer all of them together. A
task's automatic completion notice is read whole, then cancelled. A cancel after a read tells the item's sender
"[cancelled]": as a sender, read that as taken, and judge delivery by the answer. The queue isn't the only inbox: at each checkup also read the result of every child
that finished since the last one, comparing its latest run with the last one you acted on, since a result can sit
there with no notification (a lead's later turns don't wake its parent); don't poll children that are still running. Handle what arrived together as one batch: one message per agent,
and close each request with its disposition, not merely because it was read. To a busy agent, read its state first and
send one steer that holds whatever it is doing, never several corrections. Delegate work that would take more than a
few minutes of your own tool calls.
