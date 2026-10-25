---
id: rule.results-bound-to-identity
kind: rule
title: A result names exactly what produced it
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [evidence, performance]
---
Every measurement or check result names the commit it was built from (and whether the tree was clean), the toolchain,
flags, input hashes and the host. Keep the raw output and error stream. Reuse a verified compatible artifact as it is,
never relabelled as something it isn't.
