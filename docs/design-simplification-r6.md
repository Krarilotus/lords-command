# Lords Command: simplification after round 6

*25 Oct, after the sixth Sol review (Harness/notes/2026-10-25-sol-review-lords-command-r6.md). A design to review
before any code.* Rounds 4 to 6 found 7, 6 and 8 defects, two P1s each, in the same four classes (placement four
times, lint four times, attributes, swallowed errors) plus the new privacy audit. Each round closed the places it was
shown, and an inventory still missed others (import in R6-01, discovered worktrees in R6-02, the stack loader in
R6-05). The fix is the one after round 2: stop checking places one by one and make each class impossible by
construction. One rule per class, then one implementation round.

## The five rules

**A. Placement by types.** fsguard's mutating calls (put, replace, remove, mkdir, move aside, move to) take no
`&Path`. They take a `Contained`, which only `Root::join(&[Part])` makes, or an `OwnerFile` for the three owner
edits. Reads keep taking `&Path`.
- A `Part` is one validated name: not empty, no separator, not `.` or `..`, no drive, no `:`, legal on NTFS.
  `Part::skill(name)` also requires the schema's slug.
- A `Root` comes only from these constructors, each verified when it is made:
  - `Root::stack(&Stack)`: the loaded stack's dir.
  - `Root::home`: the tool's own dir.
  - `Root::target(&Target)`: a skills dir, or a `via` file's dir.
  - `Root::workspace(&Workspace)`.
  - `Root::checkout(path)`: `rev-parse --show-toplevel` equals the path, and its `.git` resolves (not prunable). Configured checkouts and discovered worktrees use the same constructor (R6-02).
  - `Root::hooks(&checkout)`: git's common `hooks/` dir.
- An `OwnerFile` comes only from a target's configured path, a settings target, or a checkout root's
  `info/exclude`.
- So every writer has to name its root. Import, promote, retire and edit join validated parts under
  `Root::stack` (R6-01: a scope's project name becomes `Part::skill`, and `../..` can't be one).
- **Set-aside** takes a human's path only through `Roots::containing(path)`. That returns one of the plan's
  already-verified roots, the one that contains the path, and the parts below it. It never builds a root from the
  path itself, which would make every path "contained"; a path in no verified root is refused.
- **Restore** re-derives every manifest line under the current config: an owner edit must be one of the
  `OwnerFile`s the config yields now, and a set-aside path must lie in one of today's verified roots. If any line
  doesn't re-derive, restore refuses before changing anything and lists each such line with its path and where its
  bytes are in the backup, for recovery by hand. It never guesses.
- The ledger stores scope plus parts, and computes the path from them, so a record is valid exactly when its root
  still verifies. That is ledger format 2. This PC's format-1 ledger holds only the 2 hook records of this
  afternoon: it is deleted and the hooks reinstalled, as a dry-run-first step.
- What A doesn't cover is code that bypasses fsguard: that is rule E's job.

**B. Removal only from a complete view.** Planning makes a `Complete` witness (a private field, so only the
planner can create one) only when the stacks and the config loaded with zero error-level problems: unreadable,
schema, layout, read, a misconfigured place. `unwanted`, `uninstall` and every `Remove` or `Forget` item need the
witness, and apply refuses a Remove that has none. A stack with any load problem still installs and updates, and
removes nothing; the plan says why (R6-05 and its neighbours). This is the class rule for swallowed errors too:
- A read error elsewhere can now only make sync write less (a conflict, a skipped install), never delete.
- The hashes that Replace and Remove compare are read strictly at apply, as today.
- The lossy readers move to `fsguard::shown`, and the lint allows `shown::` only in the display files it lists.

**C. Attributes by an allowlist.** Every replace or delete of an owner or stack file captures the attributes it
can set back (Windows: read-only, hidden, system, archive, not-indexed; Unix: the mode) and restores them on the new
file. Any other bit refuses the item: sparse, compressed, encrypted, offline, the cloud bits (pinned, unpinned,
recall on open or data access), reparse, integrity, and whatever Windows adds later.
- Promotion, whose only recovery is git, allows only ARCHIVE or NORMAL on files and plain DIRECTORY on dirs
  (R6-06: not-indexed refuses). This rule replaces the enumerated refusals.
- Measured here, read-only: the stacks, `~/.claude*`, `~/.codex`, a large product checkout and the orchestration notes carry only ARCHIVE
  (0x20), and dirs carry DIRECTORY (0x10), with `.git` dirs also hidden. Codex's own binaries carry SPARSE (0x120);
  sync never writes them.
- OneDrive is set up, in its own folder beside `Documents`, and no stack or target lies under it. A target there would
  refuse on its cloud bits, which is the right default.
- Tool-owned paths stay out of contract: the tool owns them whole, and their hash covers bytes only.

**D. Privacy reachability by behaviour.** The installed privacy hooks start with a probe that can't become a
bypass. A hook prints `lords-hook-probe:<nonce>:<hook>` and exits 0 only when `LORDS_HOOK_PROBE` is a nonce (32
hex digits) and `<home>/state/probe-<nonce>` exists. Any other value, set or exported anywhere, runs the normal
check unchanged.
- `lords check` creates that file right before it runs `git hook run --ignore-missing pre-commit` (and
  `commit-msg`, with a temp message file in the tool home), and deletes both right after. Verified means the nonce
  came back for that hook. Anything else, a git error included, is "not verified".
- That works for any dispatcher, the attribution guard or another, and reads no file to guess (R6-07, R6-08).
- Git 2.42 here has `git hook run`; an older git errors, which reads as not verified.
- What stays outside: `--no-verify`, a hooks path set after the check, and commits made elsewhere (guarantee 7's
  stated condition).
- The probe needs this repo's hooks reinstalled once. They are the tool's own (ledger-recorded), so no `--force`.

**E. Lint: the contract and its runner.**
- `tools/commit-check.sh` also runs `cargo test -p lords-core --test fsguard_lint` (R6-03, about 1 s).
- The text layer strips `r#` from identifiers before it matches (R6-04).
- The contract: it catches every ordinary spelling, raw identifiers, aliases and grouped `use` included. Deliberate
  obfuscation stays out of contract and is a matter for code review: macros that build paths, `include!`,
  generated code, `concat!`-made idents.
- Clippy's resolved lint stays the first layer, and A makes fsguard itself unable to misplace. The lint only has to
  keep runtime code inside fsguard.

## Staged first use

**What it is.** `lords sync --apply --stage install` writes only tool-owned paths (Create, Install, Update, Refresh)
plus each repo's two git-exclude lines.
- The exclude edit is the one owner edit it makes: without it `CLAUDE.local.md` shows as untracked, and every
  repo here needs it (dry run at `afbe95b`).
- It makes no Remove or Forget, no user-level owner edit, no import, retire or promote.
- It is all-or-nothing per place: a place with any conflict is skipped and listed, instead of the whole apply
  refusing.

**What it needs.** A, so its writes are placed by construction; E, so code can't bypass fsguard; and C's
capture-or-refuse rule, for the one owner file it edits. It needs neither B (it removes nothing) nor D (D is about
commits, not sync).

**What it delivers on this PC.** From a dry run at `afbe95b`, read-only:
- **Complete now:** 4 places, with 980 tool paths and their 4 exclude edits. Three small repos take 7 paths
  each; the largest, a product repo, takes 959 across its main checkout and worktrees.
- **Unlocked by the owner's adopt or set-aside step:** 7 places, each blocked by 2 pre-ledger files from the 3a
  run (`CLAUDE.local.md` and `.lords/`).
- **Waiting:**
  - one repo, for its 268 pre-ledger copies;
  - one repo, for a worktree consolidation;
  - the user-level files, which the owner edited by hand.
- **A choice for the orchestrator:** the product repo's 959 paths go into live coder worktrees, and agents there
  would read new context mid-session. Start with the three small repos, and add it when its lanes allow.

## Every finding, mapped

**Construction** means the rule makes it impossible. **Small** means fixed in place, with nothing more to do.
**Stays** means it is still open by decision, with the reason.

| Finding | What | Now |
|---|---|---|
| R3-01 | empty dir in a copy kept the hash | Small (tree hash covers dirs) |
| R3-02 | backup failure stranded the owner file | Small (backup before the move) |
| R3-03 | ledger record outside every target | Construction, A (record = scope + parts) |
| R3-04 | Unicode lowercasing merged two files | Small (NTFS upcase, file identity) |
| R3-05 | check and act are two steps | Stays: operating assumption; v2 handle-relative ops, which A's `Root` prepares (a root can hold a handle) |
| R3-06 | owner file lost read-only | Construction, C |
| R3-07 | UTF-16 without NUL bytes | Small (all readings) |
| R3-08 | lint didn't enforce its boundary | Construction, E (+ A for placement) |
| R3-09 | hooks outside the lock | Small (lock) |
| R3-10 | stack edits without a commit | Small (git keeps old bytes); placement by A |
| R4-01 | file record in a skills target, wrong adapter scope, worktree root | Construction, A |
| R4-02 | valid UTF-8 hid UTF-16 | Small (every reading) |
| R4-03 | restore lost attributes | Construction, C |
| R4-04 | no-hard-link fallback | Small (removed; seam test) |
| R4-05 | ledger format | Small (format number; A raises it to 2) |
| R4-06 | lint missed DirBuilder, aliased Command | Construction, E |
| R4-07 | promotion deleted an empty dir | Small (refused); A for its destination |
| R4 side | `check()` failed open; interrupted save | Small (strict; refusal with recovery) |
| R5-01 | ordinary dir as repo root | Construction, A (`Root::checkout`) |
| R5-02 | skill slot took any name | Construction, A (`Part::skill`) |
| R5-03 | grouped aliases, raw idents | Construction, E (clippy resolves) |
| R5-04 | unlistable state dir | Small (strict listing); harm class closed by B |
| R5-05 | promotion lost hidden | Construction, C |
| R5-06 | repeated target names | Small (names unique) |
| R6-01 | import scope escaped the stack | Construction, A (`Root::stack` + `Part`) |
| R6-02 | stale discovered worktree as root | Construction, A (one `Root::checkout` for all) |
| R6-03 | commit check didn't run the text lint | Small, E (runner runs it) |
| R6-04 | `r#` escaped the text layer | Small, E (normalise `r#`); contract stated |
| R6-05 | unreadable skill removed its copy | Construction, B |
| R6-06 | promotion lost not-indexed | Construction, C (allowlist) |
| R6-07 | audit trusted a file's comment | Construction, D (probe) |
| R6-08 | nested stack's git failure read as no repo | Construction, D (any error = not verified) |

Out of contract and stated, as before: tests, build scripts and dependencies (outside the boundary); the GUI's
`Config::discover`; deliberate lint obfuscation (E); commits that skip hooks (D); the path-swap race (R3-05).

## Cost and order

- **One implementation round, in this order:**
  1. A: the fsguard API, `Root`/`Part`/`Contained`/`OwnerFile`, ledger format 2, and the writers ported.
     Most of the work; it touches fsguard, ledger, sync, ops and hooks.
  2. B: the witness and the plan note.
  3. C: the allowlist, replacing today's refusal lists.
  4. D: the probe and the check.
  5. E: the runner and `r#`.
  6. The staged mode, last, as a flag on the existing plan.
- **Tests:**
  - Every R6 probe becomes a red-first spec test, with the reviewer's reproductions as the base.
  - The three points of the design review (25 Oct) get one each: an invented or stale probe value, with no nonce
    file, doesn't switch the check off; set-aside refuses a path in no verified root; restore refuses, changing
    nothing, a line that doesn't re-derive.
  - A's acceptance is compile-time: a test crate that tries to call fsguard's mutating API with a `&Path` must not
    build (a `compile_fail` doctest in fsguard.rs, no new dependency).
- **After it:** the ledger reset and hook reinstall on this PC, dry run first.
