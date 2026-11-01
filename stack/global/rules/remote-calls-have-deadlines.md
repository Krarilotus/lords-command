---
id: rule.remote-calls-have-deadlines
kind: rule
title: Remote calls have a deadline and fail closed
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "Every remote call has a deadline that kills its child process on expiry, remotely too: remote scripts carry their own deadline and stop themselves. A missing reading fails closed, never as \"fine\"."
visibility: public
origin:
  type: own
topics: [process]
---
Every remote call (ssh, a service API, a probe) has a deadline and kills its child process when it expires, on the
remote host as well: a script run there carries its own deadline and stops itself, since killing the local client
leaves it running. A missing reading fails closed: it is never taken as "fine".
