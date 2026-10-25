---
id: rule.mem-research-prompts
kind: rule
title: Research prompts stay open, stand alone, and are pasted in chat
audience: [all]
roles: [orchestrator, coder, helper]
scope: global
status: active
visibility: public
basis:
- quote: "when there is big uncertainty about industry standards, we shouldn't assume it, thats kinda like a rule as well, we should then have 1-2 research tasks setup in markdown codeblocks to spin off"
  date: 2026-10-20
  by: owner
evidence:
- summary: The owner wants the extra knowledge and surrounding context, to check designs from first principles, not only to confirm a chosen design
  date: 2026-10-19
- summary: The owner often sends prompts from another device (web chat, phone), so a file path alone is useless
  date: 2026-10-21
- summary: The owner removed the "write the report in English" line; their research tool already sets the report language
  date: 2026-10-21
origin:
  type: own
topics: [research, prompts]
---
Where the state of the art is uncertain, don't assume it: write one or two deep-research prompts for the owner to run.
Frame each around the problem and its neighbouring fields, not around the design already chosen; phrase it neutrally,
self-contained, and ask for named sources mapped to claims. Paste every drafted prompt or relay in the chat reply as its
own code block (give the file location in one line), and leave out any report-language line. When digesting reports,
keep findings outside the original scope and flag any that challenge a committed design.
