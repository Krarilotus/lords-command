---
id: template.code-review
kind: template
title: Code review rounds
audience: [all]
roles: [orchestrator, coder, reviewer]
scope: global
status: active
visibility: public
evidence:
- summary: Review rounds caught real deadlocks, but fix checks that widened scope ran to 9-11 rounds; a provider's safety filter ended a review worded in attack terms.
  date: 2026-10-25
origin:
  type: own
topics: [review, prompts]
---
**When.** A round starts once the head is committed and every claim has its evidence (command, exit code, log). Each
round is a fresh reviewer agent; never continue a review in the old thread. Effort: medium for a delta, tooling or
docs; high for product code; the highest where a finding could be disclosure or data loss, or for a concurrency or
crash design. Tooling that kills, pauses or admits processes on shared machines gets full rounds like product code.

**One full round per package.** After a BLOCK the next call is a fix check: scoped to the diff since the reviewed head
and the listed blocking findings, it verifies each fix and its fails-without test and raises nothing new unless a fix
introduces a blocking finding. After two fix checks that still block, stop and give the orchestrator an outer-view
summary. A second independent review only for disclosure or data-loss risk, and only with the orchestrator's OK.

**The prompt, in this order.**
1. What: repo, branch, head and base, the range, local only.
2. Read: the project's bar and its approved exceptions, the design note, every prior round's review.
3. Prior findings: each id with the author's response, fixed (commit and test) or overruled with an argument.
4. Check: numbered claims, each with file:line; ask for the failure scenario, not style.
5. The bar: every finding is classed by it; approved exceptions are named, never blocking.
6. Merge risk: a merge-tree against the current target head, plus type and signature changes that merge cleanly as
   text but break the build.
7. How: a disposable detached worktree; no timing, jobs, commits, pushes or edits to the author's worktree; each probe
   marked run (command and exit code) or not run. A reviewer who can't build says so and lists the exact commands the
   author must run.
8. Output: verdict first (MERGE, MERGE-WITH-FOLLOW-UPS or BLOCK); findings most severe first with class, file:line,
   scenario and required fix; closure of prior findings; the merge-risk list; remove the worktree and report the space
   reclaimed.

**After the verdict.** BLOCK: fix each blocking finding with a test that fails without it, or overrule it with an
argument in the next prompt. Findings are evidence, not orders: the reviewer judges the argument. Non-blocking
findings never cause a round; they become a ticket. MERGE: one message to the orchestrator with the head, the verdict
path, the classed follow-ups, timing (only where the change can affect it) and merge-risk notes. Findings beyond the
package go to the orchestrator, not into the branch. Reviews also challenge unrelated rewrites, cost shifts and
unneeded infrastructure.

**Wording.** Phrase reviews in correctness terms ("results depend only on what the reader may read", "scope
invariance", "isolation"), not attack terms. If a provider flags a review as a security risk, check that it really is
a defensive review of your own code, then restart it on a new agent with the same scope, the partial evidence and
correctness wording.
