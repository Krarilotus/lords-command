# Lords Command // one home for how you work with agents

A versioned stack of the rules, intent, roles, templates, skills and memories your coding agents read, with metadata
on every entry, a consistency and privacy check, and a sync that writes each harness's files (Claude Code and Codex
today, one adapter each) for every repo. A small native GUI on top.

Two stacks, one layout: this public repo (the tool plus a generic stack anyone can install), and your own private
stack (intent with private details, memories, host facts, per-project and per-repo rules). Entries are layered
global → project → repo → path; a narrower entry changes a broader one only by naming its id.

Status: CLI and window (milestone 4). Design: [docs/design.md](docs/design.md). Schemas: [schema/](schema/).

## > use

```text
lords check [--public] [--strict] [--json]   every check over both stacks; --public: this repo's whole tree
lords import [--store NAME]                  agents' memory dirs into the private stack, committed
lords retire ID --reason TEXT [--superseded-by ID]
lords promote ID [--strip | --split]         private → public, only once nothing private is left
lords hooks install [--secrets-only]         the privacy check as this repo's pre-commit, pre-merge-commit and commit-msg hooks
lords privacy --staged | --message FILE | FILE...
lords hash PATH                              the sha256 an inherited entry records
```

The stacks come from `--public-stack` / `--private-stack`, else `config.txt` beside the exe or in
`~/.lords-command/`:

```text
public = ~/lords-command/stack
private = ~/lords-command-private
```

## > window

`lords-command.exe` is the same, as a small native window (overseer's sibling: one line per entry, symbols and
colours, the words in the hover; `?` has the legend).

- ☰ **entries:** filter by kind, audience, layer and text; each line shows its layer tag, kind, id and title, and on the
  right where it came from (● own, ◐ adapted, ○ inherited, ↧ imported) and its worst finding. Frames: solid active,
  dashed retired; rounded public, square private. Pick one to edit its title, audience, topics and text (✓ saves and
  commits), retire it (⌫, with an optional successor), or see its history (⟲) and any commit's diff.
- ⚑ **findings:** everything `check` reports; a click goes to the entry, or to sync for a target.
- ⇄ **sync:** the dry run for the user level or one repo, each target's diff; ⇄ apply asks once more, imports memory
  first and backs up every target.
- ◫ **what each harness reads:** pick an adapter and a place; each line shows the layer and kind it comes from, and
  its entry and file line on hover.

`--view entries|findings|sync|compose`, `--pick <id>`, `--adapter <id>` and `--repo <name>` open it at a place.

## > hack

```sh
cargo test -j 2 -p lords-core --test sync    # locally: only the test you're changing (here, sync)
sh tools/fleet-test.sh                       # rustfmt, clippy -D warnings and every test, as one fleet job
```

The full check runs as a fleet job (kind `lords.test`) on the committed `HEAD`, never by hand on a working PC; it
prints the job's id, and `fleet results <id>` has each check's log and exit code. The commit hook still runs rustfmt
and clippy locally. `.\tools\package.ps1` builds the ZIP (both exes, the example config, README, LICENSE) into
`dist/`. Keep `config.txt` local.

## > known limits

- A staged submodule (gitlink) is refused by the privacy hook: it reads only file contents.
- Sync removes nothing while the stacks have any problem, warnings included, or the plan has any conflict but a copy
  the owner changed (a path from before the ledger counts): adopt or set it aside first.
- The installed hooks run the `lords` that installed them; a later build at that path is what they run then.
- `sync --stage install` imports and retires nothing, though its plan still notes the memory an apply would import.
- Sync checks a path, then acts on it by name: one operator, and nothing relinking the places during an apply
  ([design](docs/design.md), "Operating assumption").
- Before an exclude edit, sync reads `core.excludesFile`; when that git call fails for any reason, not only when it is
  unset, it uses `~/.config/git/ignore` instead.

[MIT](LICENSE)
