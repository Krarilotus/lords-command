---
id: rule.delete-only-what-you-created
kind: rule
title: Delete only what you created, by its recorded path
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
basis:
- quote: "make sure it won't happen again and take extra caution with such things"
  date: 2026-10-25
  by: owner
evidence:
- summary: An agent removed its own temp file with a pattern delete in a shared temp directory on another person's account; the pattern also matched other programs' temp files there, so what it removed is unknown
  date: 2026-10-25
origin:
  type: own
topics: [cleanup, disk, check-candidate]
---
Delete only what you created. Record each path or resource you create, and remove exactly those. Never use a glob or
pattern delete in shared locations (`/tmp`, `%TEMP%`, home directories, shared caches); give each task its own private
temp root instead. If you're not sure you created something, don't delete it: list it. A wider cleanup needs the
owner's word.
