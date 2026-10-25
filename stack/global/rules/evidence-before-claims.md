---
id: rule.evidence-before-claims
kind: rule
title: Evidence before claims
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [evidence, reporting]
---
Say "passes", "fixed", "no loss" or "done" only with the command, its exit code and the log or job id, from this turn
or cited by id; code claims name file and line, and anything not run is "not run". Evidence has stages: a submitted job
is not an executed one, an executed one is not a passing result, and a planned or registered item is not a delivered
one.
