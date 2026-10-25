# Lords Command // one home for how you work with agents

A versioned stack of the rules, intent, roles, templates, skills and memories your coding agents read, with metadata
on every entry, a consistency and privacy check, and a sync that writes each agent family's files (Claude Code,
Codex) for every repo. A small native GUI on top.

Two stacks, one layout: this public repo (the tool plus a generic stack anyone can install), and your own private
stack (intent with private details, memories, host facts, per-project and per-repo rules). Entries are layered
global → project → repo → path; a narrower entry changes a broader one only by naming its id.

Status: CLI core (milestone 2). Design: [docs/design.md](docs/design.md). Schemas: [schema/](schema/).

## > use

```text
lords check [--public] [--strict] [--json]   every check over both stacks; --public: this repo's whole tree
lords import [--store NAME]                  agents' memory dirs into the private stack, committed
lords retire ID --reason TEXT [--superseded-by ID]
lords promote ID [--strip | --split]         private → public, only once nothing private is left
lords hooks install [--secrets-only]         the privacy check as this repo's pre-commit hook
lords privacy --staged | FILE...
lords hash PATH                              the sha256 an inherited entry records
```

The stacks come from `--public-stack` / `--private-stack`, else `config.txt` beside the exe or in
`~/.lords-command/`:

```text
public = ~/lords-command/stack
private = ~/lords-command-private
```

## > hack

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

[MIT](LICENSE)
