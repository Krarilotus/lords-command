# Which instruction files each harness loads

Checked 25 Oct 2026 against the vendors' docs and with live runs (Claude Code 2.1.310, Codex CLI 0.164.0). Sync
relies on exactly these facts; re-check them when either tool changes.

## Claude Code

Source: [code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory).

- **Load order:** managed policy, then the user file `~/.claude/CLAUDE.md`, then project files.
  - Project files are `CLAUDE.md` or `.claude/CLAUDE.md`, then `CLAUDE.local.md`, in "your current working
    directory and every directory above it".
  - The files "are concatenated into context rather than overriding each other … ordered from the filesystem root
    down to your working directory"; within a dir, `CLAUDE.local.md` comes after `CLAUDE.md`.
- **`AGENTS.md`** (v2.1.277 and later) is read only when there is no `CLAUDE.md`, `.claude/CLAUDE.md` or
  `CLAUDE.local.md` "in your working directory or any directory above it".
  - The user file and managed policy don't count.
  - The setting `instructionFiles = claude-md-and-agents-md` makes Claude read both.
- **`CLAUDE.local.md`** is the per-project personal file: "add to `.gitignore`". It exists only in the worktree where
  it was created.
- **`@path` imports** expand in place, up to four hops. Project files that import a path outside the working
  directory ask for approval once; user files don't.

**`CLAUDE_CONFIG_DIR`** moves the user file. With `CLAUDE_CONFIG_DIR=<profile>`, the user file is
`<profile>/CLAUDE.md`. `~/.claude/CLAUDE.md` is then *no longer the user file*, but it still sits at
`<home>/.claude/CLAUDE.md`. So for any working directory under the home dir it loads as an ancestor project file.
Two consequences:
1. Both files load, the profile's and `~/.claude/CLAUDE.md`.
2. `~/.claude/CLAUDE.md`, being a non-empty ancestor `.claude/CLAUDE.md`, stops Claude from reading any repo's
   `AGENTS.md` under the home dir.

Live runs, with `CLAUDE_CONFIG_DIR` set to a second profile. Each project dir is a fresh git repo with marker files;
the model listed the instruction files in its context and the markers it saw:

| Project dir | Files in it | Loaded |
|---|---|---|
| under home | `AGENTS.md` | profile `CLAUDE.md`, `~/.claude/CLAUDE.md`; **not** `AGENTS.md` |
| under home | `AGENTS.md`, `CLAUDE.local.md` | both `CLAUDE.md` files and `CLAUDE.local.md`; not `AGENTS.md` |
| under home | `AGENTS.md`, `CLAUDE.local.md` containing `@AGENTS.md` | the above plus `AGENTS.md` (through the import) |
| outside home | `AGENTS.md` | profile `CLAUDE.md`, `AGENTS.md` |
| outside home, empty `.claude/CLAUDE.md` one level up | `AGENTS.md` | profile `CLAUDE.md`, `AGENTS.md` (an empty file doesn't count) |
| outside home, `.claude/CLAUDE.md` one level up importing the profile file | `AGENTS.md` | profile `CLAUDE.md` and the importing file; not `AGENTS.md`. The imported text was reported once, not twice (the model's own count; treat it as likely, not proven) |

## Codex

Source: [learn.chatgpt.com/docs/agent-configuration/agents-md](https://learn.chatgpt.com/docs/agent-configuration/agents-md)
(formerly developers.openai.com/codex/guides/agents-md).

- **Global:** `$CODEX_HOME/AGENTS.override.md` if it exists, else `$CODEX_HOME/AGENTS.md` (default `~/.codex`).
  "Codex uses only the first non-empty file at this level."
- **Project:** from the project root ("typically the Git root") down to the working directory.
  - In each dir it reads `AGENTS.override.md`, else `AGENTS.md`, else the configured fallback names, "at most one
    file per directory".
  - The files are concatenated root first, "joining them with blank lines".
- **Size:** reading stops at `project_doc_max_bytes`, 32 KiB by default, for all files together.

Live runs:
- **A dir with both files:** `AGENTS.override.md` was read and `AGENTS.md` ignored.
- **A sub-dir with its own `AGENTS.md`:** the root's then the sub-dir's.
- **The global `~/.codex/AGENTS.md`** loaded in both runs.
- **A memory index line** (`- name: hook (\`<path>\`)`) in `AGENTS.md`, pointing at a file outside the workspace:
  asked a question only that file answers, Codex read it and answered, under the read-only sandbox. Without the line
  (control), it answered "unknown".
- **An `@<path>` line is not expanded:** told not to read files, Codex didn't know a word that existed only in the
  imported file. So memory reaches Codex as an index, not an import.

## What sync does with this

The adapters (`crates/lords-core/src/adapters.rs`) encode these facts.

| Target (adapter) | File written | Why |
|---|---|---|
| user level (claude) | the profile's `CLAUDE.md` (each profile in use) | the user file |
| user level (codex) | `$CODEX_HOME/AGENTS.md` | the global file |
| a repo (claude) | untracked `CLAUDE.local.md` in every checkout and worktree | loads after the tracked files and stops a stale tracked `AGENTS.md` from loading; the stack's repo layers replace it |
| a repo (codex) | untracked `AGENTS.override.md` in every checkout and worktree | replaces the tracked `AGENTS.md` in that dir |
| a repo's tracked `AGENTS.md` | nothing: reported only | it changes by a commit on the repo's own branches, which sync never makes |

Every one of these files also carries the memory index for its layers (see the design's Memory section). Both
untracked files go into the repo's shared `.git/info/exclude`, so the worktrees stay clean. With two Claude
profiles in use, `~/.claude/CLAUDE.md` also loads under the other profile. The private stack decides which profile
file carries the global text (see the design's open question).
