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
| 3 | workspace | `workspace:<name>` | one working dir that is no git repo (e.g. a harness dir) |
| 4 | path | `path:<repo>:<subpath>` | a subtree of one repository |

Projects and repos are declared in a stack's `stack.toml`; a repo names its project, if it has one. A local checkout
path is private, so it lives only in the private `stack.toml`.

**Composition** for a context (the agents a target reaches, and optionally a repo and a path within it) takes every
active entry whose scope matches and whose audience includes those agents. It then applies the relations below and orders the result
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
| `id` | `<kind>.<slug>` | unique across both stacks; never changes meaning and is never reused (retire instead); a memory's comes from its file name |
| `kind` | intent, rule, role, fact, status, template, skill, memory | `fact` is new: host, machine and repo facts, neither rules nor memories |
| `title` | text | one line, for lists and the GUI |
| `audience` | list of opus, sol, astra, all | the agents (families' models) it is for |
| `roles` | roles from the `roles` list in `stack.toml` (e.g. orchestrator, coder, reviewer) | absent: every role. A target that knows its reader's role (`roles` on the target) gets only entries for that role or every role; where the role is unknown, role-specific entries are labelled, and role-specific notes leave the memory index for one pointer line |
| `scope` | global, `project:<p>`, `repo:<r>`, `path:<r>:<sub>` | must match the directory |
| `status` | active, retired | retired needs `retired.date` and `retired.reason`, and `superseded_by` when there is a successor |
| `visibility` | public, private | must match the stack |
| `basis` | list of {quote, date, by} | the owner's words behind the entry, where there are any |
| `evidence` | list of {summary, date, links} | the cases behind the entry (incidents, measurements); kept beside it, never composed, so the rule stays a short principle; links are checked |
| `origin` | own; adapted or inherited with repo, commit, path, licence (+ `changes` for adapted); imported with the store and file; in-repo with repo, path and hash | inherited means verbatim; in-repo: a skill tracked in a repo (`skills = [...]` on the repo), indexed where it is, never linked, copied or moved, and listed in that repo's files |
| `temporary` | {until: date \| when: {status, state}, review_by, reason} | a temporary entry: it ends on `until`, when the status entry it names reaches `state`, or when `review_by` passes unrenewed (renew by moving that date). An ended entry is never composed; `check` warns (`temporary-expired`, fails `--strict`); `sync --apply` retires it with a commit. Composed text labels a live one with its end |
| `state` | text | status entries only: the current state a `when` condition watches |
| `topics` | tags from the `topics` list in `stack.toml` | a controlled list (a public entry uses only the public stack's); drive conflict and duplicate detection |
| `overrides`, `extends` | entry ids | see Layers |
| `order` | integer | optional, for ordering within a section |
| `links` | paths or ids | checked for dangling targets |

The model is a graph: entries are nodes with stable ids, and every relation between them is a typed field
(`overrides`, `extends`, `superseded_by`, `links`, `basis`), never a position in prose. So an RDF export (for the
owner's own engine) can be added later as a pure function of the entries, with no reshaping.

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

- **It runs** in `check` and as the public repo's pre-commit and commit-msg hooks. They live in `.git/hooks/`
  (installed by `lords hooks install`), never through `core.hooksPath`, so a global hooks path that dispatches to
  the repo's own hooks (such as an attribution guard) keeps working.
- **What it reads:** each staged file's content and its path, and the commit message. Deny and fleet terms match in
  any case (Unicode lower case, NFKD), a phrase across line breaks too. UTF-16 with a byte order mark is read as
  text; other non-text files are scanned as far as their bytes read as text, and the staged check refuses them unless
  `.gitattributes` marks them `lords-binary` (e.g. `*.png binary lords-binary`).
- **Without a private stack** (CI, a stranger's clone), the deny and fleet classes are skipped. The check says so.
  A private stack that is configured but doesn't load, or a deny-list or fleet config that can't be read, fails it.
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

Each target names an **adapter** (below). Which file each harness actually loads, and in what order, is in
[load-order.md](load-order.md) (docs and live runs, 25 Oct 2026); the adapters encode exactly those facts.

**Instruction files.** The composed text goes between `<!-- lords-command:begin -->` and `<!-- lords-command:end -->`.
- Text outside the markers is the owner's and stays untouched, unless the target sets `own_text = "replace"` (used
  once a file's old text has moved into entries).
- *Drift* means the file differs from a fresh composition; `check` reports it for the user-level targets.

**User-level files** (one per adapter and per profile in use) reach every run on the machine, product runs
included (another project may start agent turns for its users with the owner's harness home). So they carry only
what suits any run: global entries for every role (no `roles`), and no memory. Development-process rules (tagged
with development roles) and the memory index go to the files of repos and workspaces, which product runs never load.

**A workspace** is a working dir with no git repo (`[workspaces.<name>] path = …` in the private stack.toml). It gets
the same untracked files as a checkout, takes the `workspace:<name>` layer, and needs no git exclude.

**A repo or workspace** (`lords sync --repo <name>`) gets, in untracked files, the global entries the user level
leaves out (development roles, memory), its project, and its own layer. Several agents may share one checkout: nothing
assumes a worktree per agent.
- Every checkout gets files, and so does every worktree under the repo's `worktree_roots` (the others are skipped
  and listed). Each gets the file its adapter's harness reads after the tracked ones: `CLAUDE.local.md` (Claude) and
  `AGENTS.override.md` (Codex).
- Both names go into the repo's shared `.git/info/exclude`, so worktrees stay clean.
- The checkout's tracked `AGENTS.md` still loads beside the stack's text, as each adapter allows: Claude's file
  starts with `@AGENTS.md` (a `CLAUDE.local.md` alone would hide it), and Codex's override carries its text inline
  (the override replaces it, and Codex doesn't expand includes). A tracked file of either name is a conflict, and
  nothing is applied.

**By reference in shared place files.** Every role reads a place's agent files whole, and Codex stops at 32 KiB
(silent truncation; the limit is never raised). So owner intent appears there as index lines (title and file, read
on demand); rules for every development role stay inline. Rules, facts, status and notes for only some roles are
one line naming the checkout's role files and how many each carries. The role files carry the full text. Index hooks
are cut at a word, at 100 characters.

**A place's default role.** A session nobody briefed (a thread the owner starts) doesn't know its role. A place's
`default_role` (stack.toml) is that role: its entries and notes go inline in the place's files, labelled; the
role-files line stays for briefed agents of the other roles.

**New worktrees.** `lords hooks install --post-checkout` writes the repo's own `.git/hooks/post-checkout` (the
common dir, so every worktree runs it). On a new worktree (old HEAD all zeros) it runs `lords sync --repo X --place
<worktree> --apply` in the background, time-boxed, and only logs (`<common dir>/lords-post-checkout.log`); any
other checkout returns at once. A hook that was there before runs first and keeps its exit status. `--place`
writes that one worktree's files, without importing or retiring.

**Shadowed `AGENTS.md`.** Claude reads an ancestor dir's `CLAUDE.local.md` and then skips a nested repo's own
`AGENTS.md` (docs/load-order.md). `check` lists every worktree that gets no files of its own but lies inside a place
(`agents-shadowed`, info).

**Index lines name each dir once.** An index (intent, skills, memory) groups its lines by dir: one line names the
dir, absolute (`In <dir>, as <name>.md:`), and each line under it gives its file only when the name doesn't. Paths
stay absolute: relative paths were dropped (orchestrator, 25 Oct), since with each dir named once they saved about
1 % of the largest place. The switch per adapter (`relative_paths`) stays off for every adapter. `lords sizes` measures every file sync writes, by section, as
written; a test against the real stacks (where a machine has them) keeps every place under the warning line.

**Role files.** Every checkout and workspace also gets `.lords/role-<role>.md` for each declared role (excluded from
git): everything that role follows there, standing alone (global, project, the place, notes), so a brief can point at
the file and carry only the task. The role `product` gets no development rule and no notes (a note for every role is
a development note).

**Nothing of a tracked `AGENTS.md` is dropped.** Codex's override carries it verbatim, then the stack's layers;
Claude's local file includes it with `@AGENTS.md`. `check` verifies both in every checkout and workspace: every
line of `AGENTS.md`, in order, in the override, and the include in the local file.

**A repo's tracked `AGENTS.md`** is never written: it changes by a commit on the repo's own branches. Sync reports
what the public layers (public global, project and repo) would make of it, so the maintainer can commit that.

**Skills** go into each adapter's skills dir as copies, never links (orchestrator, 25 Oct: a `git worktree remove
--force` on Git for Windows 2.42 followed a skill junction and emptied the stack's skill dirs; symlinks need admin
rights or developer mode on Windows). Each copy carries an ownership record, `.lords/owned.toml`: the stack entry's
id, its source dir and a SHA-256 per file. Built beside its place and renamed in, a copy is whole or absent. Its exact
path is excluded from git, and `check` sizes the copies per place (about 33 KB per checkout today). Deleting it, with its worktree say, never reaches the stack.
Sync refreshes a copy nobody changed when the stack's skill changes (the old one moves to the backup); a copy changed by
hand is a conflict. A copy of a skill that no longer applies moves to the backup; nothing is deleted. A link of ours
from an earlier version gives way to a copy (the link itself is removed, never recursively). A real dir without a
manifest is the tool's only if it is a copy of a stack skill: its SKILL.md names it, it holds no link, and no file
the stack's copy lacks. Every other dir stays. A real dir where a copy should go is moved to the backup first, and
its diff is shown. Walks never follow a link. Codex reads the "Skills here" index in each place's file.

**What apply may touch** (the Sol review of 25 Oct): only the configured path, as a plain file. A target that is a
link, or lies below one, is a conflict; so is one sync can't read as UTF-8, and a file whose marker lines aren't one
begin line before one end line (markers count only as whole lines). Writes go through a new file renamed over the
target, so nothing is ever half-written and a hardlink keeps its old bytes. Every byte outside the managed block
stays, the line ending after the end marker included; a settings file changes only in its one key. Apply first
checks every path is still as planned and refuses the whole plan otherwise. One apply runs at a time
(`backups/apply.lock`).

**Backups.** Each `--apply` gets a new dir, `~/.lords-command/backups/<date>-<pid>-<n>/`: replaced files under
`files/` and moved dirs under `dirs/`, each at its whole path, and `manifest.tsv`, one line per change written before
the change (including where each removed link pointed, and each file and link it created). `lords sync --restore
<dir>` undoes it, last change first, leaving anything changed since. It refuses a plan with any conflict, before any
stack file changes; import and retirement run only on a clean git checkout of their stack.

**Where each line comes from.** Composition keeps a map: output line → entry id → stack, file and line. The GUI
shows it; `sync --json` prints it. The agent files carry only the text, plus one header comment naming the source.

**Audience.** Sol and Astra both read `~/.codex`, so a user-level Codex file can't tell them apart. An entry for
only one of them is labelled ("(Sol only)") where it stands. Opus is reached through the Claude adapter.

**Several profiles of one harness.** Targets may share one composed file: `via = "<file>"` makes the target's own
file hold only an include line (adapters whose harness expands includes). A `settings` target writes the adapter's
exclude list (Claude: `claudeMdExcludes`). For two Claude profiles: both `CLAUDE.md` files include the shared
file, and the second profile excludes `~/.claude/CLAUDE.md`, which would otherwise load again as an ancestor and
hide repos' `AGENTS.md`. Without that exclude, the plan (and `check`, as info) says so.

## Ownership boundary

*Proposed 25 Oct after the second Sol review; once built, it replaces the Sync paragraphs on skills, on what apply
may touch, and on backups.* Two review rounds found the same class again and again: the tool *inferred* what was
its own (a dir by its name or contents, a record inside a copy, a link by where it points) and *edited in place*
on a filesystem the owner also uses. Each patch closed one case and opened its neighbour. So sync stops inferring.

**Three kinds of path.**
1. **Tool-owned paths**, the only ones sync creates, replaces or removes: each skill copy dir, each place's `.lords/`
   dir, `CLAUDE.local.md`, `AGENTS.override.md`, and the files under `~/.lords-command/out/`. Each is whole: sync never
   edits one, it builds the new version beside it and swaps it in.
2. **Owner files**, which sync edits minimally and nowhere else: the managed block in the user-level instruction files
   (`~/.claude/CLAUDE.md`, a profile's `CLAUDE.md`, `~/.codex/AGENTS.md`), the one settings key (`claudeMdExcludes`),
   and sync's two lines in a repo's `.git/info/exclude`. Only these need backups and restore.
3. **Everything else**, which sync only reads, and never through a link.

**The ledger** (`~/.lords-command/state/ledger.toml`) is the only proof of ownership. It lists every tool-owned path
sync created: its normalized path (case-folded on Windows), what it is, and the SHA-256 of what sync put there (a
dir's hash covers every file in it). A path that isn't in the ledger is not the tool's, whatever its name or
contents: where sync wants to write and finds one, that's a conflict. Nothing in a copy says it's the tool's; there is
no record to parse or forge, and no directory listing decides what goes. What goes is what the ledger holds and the
stack no longer wants.

**Replacing a tool-owned path.** Sync builds the new version beside it, under a name unique to the run, created with
`create_new`; it hashes what it built, not the source. It checks the old version still matches its ledger hash,
moves it aside, renames the new one in, records the new hash, then deletes the old one. Rollback is regenerating
(sync from an earlier stack commit) or `lords uninstall`, which removes every ledger path that still matches; never a
restore into a dir the owner may have touched.

**Removing** happens only to a ledger path whose hash still matches, through one delete that follows nothing: it
refuses a reparse point (junction, symlink) on the path or anywhere below it, and deletes file by file, bottom up. A
path that changed is left and reported.

**Every filesystem step goes through one choke point** that refuses a reparse point anywhere from the drive root
down to the path (ancestors included) before it reads, writes, moves or deletes there.

**Owner-file edits** read the file, compute the new bytes, then move the file aside into the run's backup and compare
what was moved with what was planned. A mismatch means an edit since planning: the file goes back and that item
fails, so nothing an owner wrote is lost, even during the apply. Otherwise the new file is renamed in. Backups are
numbered files with a manifest naming each original path and the hash sync wrote. `restore` puts a file back only
while it still holds exactly what sync wrote; anything else is left and reported.

**Stack edits** (import, retirement) touch only files git already tracks and holds unmodified, plus new files; they
run under the same lock, after the plan is known to apply.

**The changeover, once and with a human:** the 3a place files, the old `~/.codex/skills` copies and any other path
from before the ledger are listed by `lords adopt --list`. Adopting a file records it only if it is byte for byte what
sync would write now. Removing an old copy moves it into the backup only on the owner's confirmation.

**Out of git without editing git.** A tool-owned dir (a skill copy, `.lords/`) holds a `.gitignore` of `*`, so it
ignores itself. Only the two root files need lines in `.git/info/exclude`.

## Adapters

The stack format belongs to no harness: entries are Markdown with neutral metadata, layers are directories, and
nothing in an entry names a harness's file or convention. Claude Code and Codex are two sync **targets**, each
written by one adapter (`crates/lords-core/src/adapters.rs`). An adapter says:
- which agents its files reach by default (Claude: Opus; Codex: Sol, Astra);
- which untracked file its harness reads in a repo (`CLAUDE.local.md`, `AGENTS.override.md`);
- how much instruction text the harness reads in total (Codex: 32 KiB), so sync can warn;
- how composed text is written for it (both write the same Markdown today).

`stack.toml` names adapters by id (`adapter = "codex"` on a target, `adapters = [...]` on a repo). A third harness
is one more adapter and its facts in load-order.md; the stacks don't change.

## Memory

Agents keep their own memory dirs as the write path (Claude's auto-memory, for one). Lords Command reads them; it
never writes memory back.
- **`import`** copies each store's notes verbatim into the private stack's `<layer>/memory/<store>/` and commits,
  so memory has history. A store's `scope` puts it in a layer; its `audience` (default all) says who may read it.
- **Every adapter's output carries a memory index:** the user-level files for global notes, each repo's files for its
  project and repo notes, filtered by audience. One line per note: name, the note's one-line hook, and the path of
  its copy in the stack.
  - Harnesses read a note's file when its topic comes up. Codex was checked to do this; it doesn't expand `@path`
    imports, so an index is the form every harness can use (load-order.md).
  - A store that a harness already loads by itself may be marked `native = ["<adapter>"]` and left out of that
    adapter's index, so it isn't read twice. Usually it isn't: the index reaches sessions the harness's own memory
    doesn't.
  - Notes for one role (`roles`, e.g. orchestrator) are left out wherever the reader's role is unknown (a user-level
    file, a repo's files); one line says how many there are per role and where. A target that names its role lists
    them.
- **`sync --apply` imports first** (one commit), then writes, so the index points at the notes as they are. A dry run
  says how many notes an import would bring.
- **A note binds nobody.** The index is introduced as notes, not rules. Making a note binding is a promotion: it
  becomes an intent, rule or fact entry, and the memory entry is retired with `superseded_by`.

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
| an adapter's composed text past 75 % of its size limit (a user-level file; a repo's files plus the user level) | warning |
| an entry naming a topic or role its stack doesn't list | error |
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
