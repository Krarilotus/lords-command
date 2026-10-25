# Lords Command // one home for how you work with agents

A versioned stack of the rules, intent, roles, templates, skills and memories your coding agents read, with metadata
on every entry, a consistency and privacy check, and a sync that writes each agent family's files (Claude Code,
Codex) for every repo. A small native GUI on top.

Two stacks, one layout: this public repo (the tool plus a generic stack anyone can install), and your own private
stack (intent with private details, memories, host facts, per-project and per-repo rules). Entries are layered
global → project → repo → path; a narrower entry changes a broader one only by naming its id.

Status: design (milestone 1). See [docs/design.md](docs/design.md) and [schema/](schema/).

[MIT](LICENSE)
