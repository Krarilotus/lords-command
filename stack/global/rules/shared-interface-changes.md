---
id: rule.shared-interface-changes
kind: rule
title: Tell the owner before changing a shared interface
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: core
core: "Before changing a type, signature or contract another package uses, tell its owner and the orchestrator with a concrete proposal. Each shared type has one owner; others ask it for changes."
visibility: public
origin:
  type: own
topics: [design]
---
**Purpose:** a shared type changed by one user breaks the others, and two owners of one type diverge.

**Default:** before changing a type, signature or contract that another package uses, tell its owner and the
orchestrator, with a concrete proposal. Each shared type has one owner; others ask it for changes.

**When to depart:** a change that keeps the contract exactly (an added private helper, a doc fix) needs no proposal.
Anything a caller could notice still does.
