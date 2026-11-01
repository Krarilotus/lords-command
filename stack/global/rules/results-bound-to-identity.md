---
id: rule.results-bound-to-identity
kind: rule
title: A result names exactly what produced it
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "Every measurement or check names commit, tree cleanliness, toolchain, flags, input hashes and host, and keeps raw stdout and stderr. Reuse verified compatible artifacts as they are, never relabelled as something they are not."
visibility: public
origin:
  type: own
topics: [evidence, performance]
---
Every measurement or check result names the commit it was built from (and whether the tree was clean), the toolchain,
flags, input hashes and the host. Keep the raw output and error stream. Reuse a verified compatible artifact as it is,
never relabelled as something it isn't.
