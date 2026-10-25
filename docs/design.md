# Lords Command: design

Status: milestone 1 (schema and layout), 25 Oct 2026. Later milestones refine the sections marked *to verify*.

## Problem

Someone working with several agent families (Claude Code, Codex, …) ends up with their rules in many places:
instruction files per family and per machine profile, per-repo `AGENTS.md` copies on many branches, memory dirs only
one family reads, skills copied by hand, and briefs that restate rules in their own words. The copies drift. Rules
contradict each other, stale facts persist, and the owner's intent reaches some agents only second-hand. Nothing
keeps history or says where a skill came from.

Lords Command is one versioned home for all of it, with:
- metadata on every entry;
- a checker that finds conflicts and leaks;
- a sync that writes what each agent reads, for each repo;
- a small native GUI.

## Two stacks

| Stack | Repo | Holds | Readers |
|---|---|---|---|
| **public** | `lords-command` (this repo, MIT) | the tool, the schema, generic rules, templates and skills, with provenance, plus docs | anyone who installs it |
| **private** | the owner's own repo, e.g. `lords-command-private` (private on GitHub) | owner intent with private details, memories, host and machine facts, project and repo rules, targets with local paths, the privacy deny-list | the owner's machines |

Both stacks share one on-disk layout (below), so an entry moves between them by moving a file.

The public stack works alone: someone else installs `lords-command`, adds an empty private stack (`lords init`), and
gets a working setup. The public stack never refers to a private id (checked).

The private stack is private but **not a secret store**: tokens, keys and webhook secrets belong in neither stack.
The secret part of the privacy check runs on both.

## Layers: how narrow an entry is

| Rank | Layer | `scope` value | Applies to |
|---|---|---|---|
| 0 | public global | `global` (in the public stack) | all agents, all work |
| 1 | private global | `global` (in the private stack) | all of the owner's work |
| 2 | project or domain | `project:<name>` | every repo of that project |
| 3 | repo | `repo:<name>` | one repository |
| 4 | path | `path:<repo>:<subpath>` | a subtree of one repository |

Projects and repos are declared in a stack's `stack.toml`; a repo names its project, if it has one. A local checkout
path is private, so it lives only in the private `stack.toml`.

**Composition** for a context (agent family, and optionally a repo and a path within it) takes every active entry
whose scope matches and whose audience includes that family. It then applies the relations below and orders the result
by section (intent, roles, rules, facts, status), then layer, then `order`, then id.

**Narrower layers change broader ones only by explicit id:**
- `overrides: [id, …]`: the named entries are dropped from this composition and this entry takes their place. The
  target must sit at the same or a lower rank. Overriding a narrower entry is an error.
- `extends: [id]`: this entry's text is placed directly after the named entry (adds detail, keeps the original).
- Two entries with the same id are an error, in any layers or stacks.
- **An unnamed conflict** is two applicable entries that share a `topics` tag and disagree (different numbers for
  the same quantity, opposite keywords such as always/never, allowed/forbidden, default N vs no N), where neither names
  the other. `check` reports it as a **candidate for the owner**, with both texts. It never picks a side.
- `check` also lists every override, so the owner sees what each layer replaces.

The rule "every line has one home" becomes checkable: one id, one file, one layer; a narrower variant names the line
it replaces.

## On-disk layout (both stacks)

```
<stack>/
  stack.toml                  name, visibility, projects, repos (+ targets and memory stores: private only)
  global/
    intent/  rules/  roles/  facts/  status/  templates/  memory/
    skills/<name>/            SKILL.md as the agent reads it, plus lords.yaml (metadata) and LICENSE where needed
  project/<project>/          same kind dirs
  repo/<repo>/                same kind dirs; path-scoped entries also live here
  privacy/                    public: allow.txt (false positives); private: deny.txt
```

- **A plain entry** is one Markdown file with YAML frontmatter (schema: `schema/entry.schema.json`).
- **A skill** is a directory. Its `SKILL.md` stays exactly what the agent reads; an inherited skill stays byte for
  byte its source. The skill's metadata goes in a sidecar `lords.yaml` in that directory, with the same fields as the
  frontmatter, and covers every file in the directory.
- **An imported memory store** keeps each memory file verbatim, as the agent wrote it. A `lords.defaults.yaml` in the
  store's directory supplies the fields every file in it shares (kind, scope, audience, visibility, origin). Each
  file's own `name` becomes its id (`memory.<name>`).
- `check` verifies that the directory and the metadata agree: the kind dir matches `kind`, the layer dir matches
  `scope`, and the stack matches `visibility`.

## Entry metadata

| Field | Values | Notes |
|---|---|---|
| `id` | `<kind>.<slug>` | unique across both stacks; never reused after retirement |
| `kind` | intent, rule, role, fact, status, template, skill, memory | `fact` is new: host, machine and repo facts, neither rules nor memories |
| `title` | text | one line, for lists and the GUI |
| `audience` | list of opus, sol, astra, all | |
| `scope` | global, `project:<p>`, `repo:<r>`, `path:<r>:<sub>` | must match the directory |
| `status` | active, retired | retired needs `retired.date` and `retired.reason`, and `superseded_by` when there is a successor |
| `visibility` | public, private | must match the stack |
| `basis` | list of {quote, date, by} | the owner's words behind the entry, where there are any |
| `origin` | own; adapted or inherited with repo, commit, path, licence (+ `changes` for adapted); imported with the store and file | inherited means verbatim |
| `topics` | tags | drive conflict and duplicate detection |
| `overrides`, `extends` | entry ids | see Layers |
| `order` | integer | optional, for ordering within a section |
| `links` | paths or ids | checked for dangling targets |

An entry's file in git *is* its history: the GUI shows `git log --follow` and diffs per entry. Nothing is deleted.
`retire` sets the status, and a retired entry stays in the tree. Composition skips it, and `check` flags anything
that still points at it.

## Privacy

A **finding** is a matched span: file, line, pattern class and the matched text. The classes are:

| Class | Matches | Runs on |
|---|---|---|
| secret | tokens and keys (known prefixes, high-entropy assignments), URLs with credential query parameters, webhook URL shapes | both stacks |
| path | Windows and WSL user paths (`X:\Users\<name>`, `/mnt/<d>/Users/<name>`, `/c/Users/<name>`, `/home/<name>`) | public |
| contact | email addresses | public |
| fleet | host names read from the fleet config that the private `stack.toml` names | public |
| deny | each line of the private `privacy/deny.txt` (names, projects, hosts, phrases) | public |

- **It runs** in `check` and as the public repo's pre-commit hook. That hook lives in `.git/hooks/` (installed by
  `lords hooks install`), never through `core.hooksPath`, so a global hooks path that dispatches to the repo's own
  hooks (such as an attribution guard) keeps working.
- **Without a private stack** (CI, a stranger's clone), the deny and fleet classes are skipped. The check says so.
- `privacy/allow.txt` lists accepted false positives as exact strings.
- **Promotion** (private → public) is one explicit command, `lords promote <id>`:
  1. it runs the check on the entry;
  2. each finding is either stripped (replaced by a generic phrase the owner edits) or split out: the private
     detail moves into a new private entry that `extends` (or `overrides`) the public one;
  3. the move happens only when no finding is left.

  Composition then gives the owner the same text as before, and the public copy is clean.

## Sync

`sync` is a dry run unless `--apply` is given. It shows the diff per target and backs up every target before writing.

Targets are declared in the private `stack.toml`, since their paths are private. The public repo ships the generic
defaults (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, `~/.claude/skills`, `~/.codex/skills`).

**Instruction files.** The composed text goes between `<!-- lords-command:begin -->` and `<!-- lords-command:end -->`.
Text outside the markers is the owner's and stays untouched. *Drift* means the block differs from a fresh composition,
or the file changed outside the tool.
- **User-level files** (one per agent family and profile) get public global plus private global.
- **A repo target** (`lords sync --repo <name>`) gets the full composition for that repo: public global, private
  global, project, repo, and path.
- **Tracked repo files.** A repo's tracked `AGENTS.md` is shared with everyone who clones it, so it receives only
  entries that are public. The full composition goes into an untracked file the family reads, listed in
  `.git/info/exclude`. *To verify in milestone 3:* which untracked file each family reads per repo (Claude:
  `CLAUDE.local.md`; Codex: `AGENTS.override.md`).

**Skills** go into each family's skills dir as directory links (junctions on Windows, symlinks elsewhere), never
copies. Each link records its owner in a `.lords-command` marker, so sync touches only what it made.

**Where each line comes from.** Sync writes a map next to its state (`~/.lords-command/out/<target>.map.json`):
output line → entry id → stack, file and line. The GUI uses it. The agent files carry only the text, plus one header
comment naming the source.

**Audience.** Sol and Astra both read `~/.codex`, so a user-level Codex file can't tell them apart. Entries for only
one of them are composed into a labelled subsection ("Sol:", "Astra:"). Opus is the Claude family.

## Import and memory

`import` copies what agents wrote into their memory dirs into the private stack's `memory/<store>/` and commits it,
so memory has history. Agents keep writing to their own dirs, which stay live. Lords Command doesn't write memory back.

Memory that should bind every agent is promoted by hand: it becomes an intent, rule or fact entry, and the memory
entry is retired with `superseded_by`.

## Check

`check` runs every test below and exits non-zero on any error. Candidates are reported but don't fail by default
(`--strict` makes them fail).

| Check | Severity |
|---|---|
| frontmatter and sidecars valid against the schema; directory, kind, scope and visibility agree | error |
| id defined once across both stacks; `overrides`/`extends`/`superseded_by`/links resolve; overrides point broader or equal | error |
| public stack refers to a private id | error |
| adapted or inherited entry without repo, commit, path and licence; inherited skill differs from its recorded source hash | error |
| privacy findings (public stack: all classes; private stack: secret) | error |
| deployed target drifted from a fresh composition | warning |
| near-duplicate text across entries (shingled similarity over normalised sentences) | candidate |
| same topic, conflicting numbers or keywords, no relation named | candidate |
| active entry links a retired one | warning |

## Binaries

One Cargo workspace with three crates:
- `lords-core` (library): model, loading, composition, check, privacy, sync plan. It calls git through the `git`
  CLI, so the user's hooks and config apply.
- `lords` (console binary): `check`, `sync`, `import`, `retire`, `promote`, `hooks install`, and later `export`.
- `lords-command` (GUI binary, eframe like overseer): it links `lords-core` directly.

**Why two binaries.** On Windows an exe is either a console or a GUI program. A single exe would flash a console
window for the GUI or lose CLI output in hooks and scripts. The split also keeps the CLI free of the GUI's
dependencies: it is what the pre-commit hook runs, so it should be small and quick to start.

The GUI never shells out to the CLI. Both call the same library, so they can't disagree.

Configuration is overseer's style: `key = value` lines in `config.txt`, read from beside the exe, else from
`~/.lords-command/config.txt`. The main keys are `public` and `private` (the stack paths).

## Non-goals

- **No secret storage**, and no syncing across machines beyond git itself.
- **No automatic conflict resolution.**
- **No editing of a deployed file:** a change goes into the stack, then `sync`.
