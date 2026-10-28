# Lords v2, slice 1: selected stores, delivery and audit

Implements slices 1a and 1b of the approved design (`Harness/notes/2026-10-28-lords-design-v2.md`, §2, §4, §5, §7).
This note fixes the interfaces the parts share and the inputs the tests attack. The design record is the source of
the behaviour; where this note and the design differ, the design wins and this note is wrong.

## Parts and owners

| Part | File | Contract |
|---|---|---|
| Registry and identity | `lords-core/src/identity.rs` | `Registry::load(home)`, `register_store(home, store)`, `resolve(&Registry, cwd, hint) -> Binding` (design §2.1, identity algorithm) |
| Lifecycle | `lords-core/src/model.rs` (`Entry::lifecycle`), `temporary.rs`, `check.rs`, `compose.rs`, `schema/entry.schema.json` | one evaluator; everyone else consumes its result (design §4) |
| Selected loading | `lords-core/src/stack.rs` | `Stack::load_selected(root, scopes)`, `Stacks::selected(&Registry, &Binding)`: core = public `global` + private `global` only; plus the bound project's store. Nothing else is opened. |
| Agent API | `lords-core/src/serve.rs` | `context`, `search`, `remember`, `brief` as plain functions returning serde values; CLI and MCP only print them |
| Journal | `lords-core/src/journal.rs` | append-only JSONL under the registry home, locked appends, no bodies or secrets |
| Store migration | `lords-core/src/migrate.rs` | copies one project's layers from the old private stack into a new store with an id/hash manifest; never edits the source |
| CLI | `crates/lords/src/main.rs` | `brief`, `context`, `search`, `remember`, `check --store`, `audit`, `register`, `store migrate` |
| MCP server | `crates/lords-mcp` | stdio via `rmcp`; tools `context`, `search`, `remember` call `serve` |

## Registry home

`$LORDS_HOME`, else `~/.lords`. It holds `stores.toml` (store locations and approved bindings) and `journal/`.
Tests always set their own home; nothing here touches `~/.lords-command` (the installed v1 tool) or any harness config.

```toml
[core]
public = "C:/…/lords-command/stack"
private = "C:/…/lords-command-private"   # only its global/ layer is read

[[stores]]
project = "alpha"
path = "C:/…/alpha-lords"

[[repos]]            # written by `lords register`, never by hand
project = "alpha"
name = "app"
common_dir = "C:/…/alpha/app/.git"   # canonical, absolute
checkout = "C:/…/alpha/app"                                       # canonical
remote = "github.com/org/repo"                                 # optional, credential-free

[[workspaces]]
project = "alpha"
name = "alpha"
path = "C:/…/alpha"            # canonical
descendants = [".local/notes"]   # non-git subdirs that inherit it; none by default
```

## Project store format

A directory (its own private git repo) laid out as a stack today:

```text
stack.toml          name, visibility = "private", project = "<id>", roles, topics, [repos.*], [workspaces.*], [[memory_stores]]
project/<id>/<kind>/…      entries scoped project:<id>
repo/<name>/<kind>/…       entries scoped repo:<name>, for repos the manifest declares with project = "<id>"
workspace/<name>/<kind>/…  likewise for workspaces
```

A store holds no `global/` layer and no entry of another project; `check --store` reports either as an error.
New memory from `remember` lands in `project/<id>/memory/remembered/<slug>.md` with full frontmatter.

## Inputs reused for 1b and 4

- The fresh-session probes at each real start location (`Harness/scratch/lords-command-eff/delivery-20261027/`:
  `claude-replica.sh`, `real-starts.sh`, `claude-rules-agents.sh`, `codex-replica.sh`, `claude-lazy3.sh`) and the load
  rules they measured (`Harness/notes/2026-10-27-lords-delivery-and-plan-time.md`, D1–D4): the 1b delivery matrix.
- Codex's user-file budget: openai/codex `0d68383`, `core/src/agents_md.rs:68-92, 142-178` (the user file is read
  whole), and the v1 size guard (`OverLimit`, `b6edcf6`): the 1b Codex staging stays under it.

## Inputs the tests attack (per invariant)

- **Identity:** a cwd reached through a junction; a linked worktree outside any declared root; a nested unrelated git
  repo inside a bound checkout; a submodule; a fresh clone of the same remote; a non-git subdir of a workspace, listed
  and not listed; an unknown dir; a dir that doesn't exist; two stores claiming one project; two projects claiming one
  common dir; a `--project` hint that disagrees; a remote URL with embedded credentials.
- **Isolation (F1 witness):** another project's store that is broken and unreadable sits beside the bound one: every
  command for the bound project succeeds and the other store's hash is unchanged; an entry linking the other
  project's id is a dangling-link error.
- **Lifecycle:** `until` on the day and the day before; `review_by` on the day and the day after; review-only;
  `when` with a missing target; malformed and impossible dates (`2026-02-30`); blank reason; already-expired write.
- **Remember:** concurrent writers on one store; the same title twice; a title that slugs to nothing or to `..`;
  a helper or reviewer role; an unknown cwd; a kind other than memory/fact.
- **Hook environment:** `brief` run with `GIT_DIR`, `GIT_WORK_TREE` and `GIT_INDEX_FILE` naming another repo resolves
  the same binding and reads the same stores (the path `eedabce` covers: `process::git` clears git's repo variables).
- **Delivery:** a delivery id from another project or another role; an unknown or malformed id; two sessions in one
  cwd with different roles, one calling context; a failed or truncated response; MCP down, CLI fallback.
