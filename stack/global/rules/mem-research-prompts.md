---
id: rule.mem-research-prompts
kind: rule
title: Research prompts stay open, stand alone, and are pasted in chat
audience: [all]
roles: [orchestrator, coder, helper]
scope: global
status: active
start: role
core:
- line: "To check designs from first principles: where the state of the art is uncertain, give the owner one or two neutral, self-contained research prompts on the problem and its neighbouring fields (skill.research-prompt), each pasted in chat as its own code block with its file in one line; keep out-of-scope findings and flag those that challenge a committed design."
  roles: [orchestrator, coder, helper]
visibility: public
basis:
- quote: "when there is big uncertainty about industry standards, we shouldn't assume it, thats kinda like a rule as well, we should then have 1-2 research tasks setup in markdown codeblocks to spin off"
  date: 2026-10-20
  by: owner
  source: "recorded in the rule corpus, not reproduced in his intent register; its original source is not independently verified (corpus audit 8 Nov, U1)"
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
links: [skill.research-prompt]
---
**Purpose:** an assumed state of the art leads to designs that confirm themselves; the owner wants the surrounding
knowledge, and he often runs prompts from another device.

**Default:** before drafting a prompt or a design from scratch, ask the project's existing research (its research
wiki, where it has one) what is already known. Where the state of the art is still uncertain, don't assume it: write one or two deep-research prompts for the owner to run.
Frame each around the problem and its neighbouring fields, not around the design already chosen; phrase it neutrally,
self-contained, and ask for named sources mapped to claims. Paste every drafted prompt or relay in the chat reply as its
own code block (give the file location in one line), and leave out any report-language line. When digesting reports,
keep findings outside the original scope and flag any that challenge a committed design. Named sources are mapped to
claims. Drafted relays to other threads are pasted the same way. The writing method is skill.research-prompt.

**When to depart:** existing research that already answers the question is reused instead; say which.
