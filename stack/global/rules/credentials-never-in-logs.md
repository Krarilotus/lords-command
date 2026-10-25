---
id: rule.credentials-never-in-logs
kind: rule
title: Credentials never reach logs or commits
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
origin:
  type: own
topics: [security]
---
Treat webhook URLs, tokens and keys as credentials: never echo, log, store or commit them, and never pass one service's
credentials to another.
