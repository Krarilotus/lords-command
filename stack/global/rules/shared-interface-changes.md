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
Before changing a type, signature or contract that another package uses, tell its owner and the orchestrator, with a
concrete proposal. Each shared type has one owner; others ask it for changes.
