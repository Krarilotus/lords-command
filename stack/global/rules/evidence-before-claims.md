---
id: rule.evidence-before-claims
kind: rule
title: Evidence before claims
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "To keep claims checkable: \"passes\", \"fixed\", \"no loss\" or \"done\" cite the command, exit code and log or job id, and the result names what produced it (commit and clean tree, toolchain, flags, input hashes, host; raw output kept); code claims name file and line, unrun is \"not run\", and submitted, run, passed and delivered stay apart. Reuse a verified result as it is, never relabelled; a published number is a hypothesis until measured here."
visibility: public
origin:
  type: own
topics: [evidence, reporting, performance]
---
**Purpose:** a claim someone else acts on must be checkable, and a result must name exactly what produced it.

**Default:** say "passes", "fixed", "no loss" or "done" only with the command, its exit code and the log or job id,
from this turn or cited by id; code claims name file and line, and anything not run is "not run". A submitted job is
not an executed one, an executed one is not a passing result, and a planned item is not a delivered one.

Every measurement or check result names the commit it was built from (and whether the tree was clean), the toolchain,
flags, input hashes and the host, and keeps the raw output and error stream. Reuse a verified compatible artifact as it
is, never relabelled as something it isn't. Count work before timing it. A published result is a hypothesis until it
is measured here; a source's claim may be cited as that source's, attributed. Record kept and rejected ideas alike.
(Merged 9 Nov from rule.results-bound-to-identity and rule.measure-before-believing.)

**When to depart:** where an authoritative result record (a job id, a gate receipt) already carries these fields,
cite it instead of repeating them. Never present an unrun or inconclusive check as a pass.
