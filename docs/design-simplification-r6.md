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

## Where each finding is closed, at `2eb6b9d`

Paths are below `crates/lords-core/` unless they start otherwise. "Test" names the spec test that shows it.

| Finding | Closed by | Where | Test |
|---|---|---|---|
| R3-01 | small: tree hash covers dirs | `src/fsguard.rs:44` (`tree_sha`), `:176` (`read_tree`) | `round3::r3_01_*` |
| R3-02 | small: backup before the move | `src/sync.rs:1906` (`owner_update`) | `round3::r3_02_*` |
| R3-03 | A: record = verified root + slot parts | `src/sync.rs:474` (`placed`), `:512` (`check_ledger`); `src/ledger.rs:16` (format 2) | `round3::r3_03_*`, `round4::r4_01_*` |
| R3-04 | small: NTFS upcase, file identity | `src/ledger.rs:46` (`key`), `:73` (`upcase`) | `round3::r3_04_*` |
| R3-05 | stays: operating assumption; post-action check | `src/fsguard.rs:474` (`verify_inside`) | `round3_api::r3_05_*` |
| R3-06 | C: keep what can be set back, refuse the rest | `src/fsguard.rs:339` (`keep`), `src/sync.rs:1906` | `round3::r3_06_*` |
| R3-07 | small: every reading | `src/privacy.rs:335` (`readings`) | `round3::r3_07_*` |
| R3-08 | E: clippy + text layer, run by the commit check | `clippy.toml`; `tests/fsguard_lint.rs`; `tools/commit-check.sh:10` | `fsguard_lint::*` |
| R3-09 | small: hooks take the lock | `src/hooks.rs:212`; `src/sync.rs:1776` (`ApplyLock::take`) | `round3::r3_09_*` |
| R3-10 | small: git keeps the old bytes; A places the edit | `src/ops.rs:45` (`git_keeps`), `:87` (`stack_place`) | `round3::r3_10_*` |
| R4-01 | A: slots over verified roots | `src/sync.rs:474`, `:1184` (`checkouts`); `src/fsguard.rs:751` (`Root::checkout`) | `round4::r4_01_*` (3) |
| R4-02 | small: every reading | `src/privacy.rs:335` | `round4::r4_02_*` |
| R4-03 | C: restore sets the recorded attributes back | `src/sync.rs:2112` (`restore`) | `round4::r4_03_*` |
| R4-04 | small: hard link only | `src/fsguard.rs:272` (`link_in`), `:282` | `sync.rs` unit test (seam) |
| R4-05 | small: format, now 2 | `src/ledger.rs:16` | `round4::r4_05_*` |
| R4-06 | E | as R3-08 | `fsguard_lint::r4_06_*` |
| R4-07 | small: empty dir refuses | `src/ops.rs:617` | `round4::r4_07_*` |
| R4 side | small: strict check; save cut short refuses | `src/fsguard.rs:62` (`check`); `src/ledger.rs:146` | `round4::check_*`, `round4::an_interrupted_*` |
| R5-01 | A: `Root::checkout` | `src/fsguard.rs:751`; `src/sync.rs:1184` | `round5::r5_01_*` (3) |
| R5-02 | A: `Part::skill` in the slots | `src/fsguard.rs:685` | `round5::r5_02_*` (2) |
| R5-03 | E: clippy resolves | `clippy.toml` | `fsguard_lint::r5_03_*` |
| R5-04 | small: strict listing; B for the harm | `src/fsguard.rs:110` (`list_dir`); `src/stack.rs:185` | `round5::r5_04_*` |
| R5-05 | C: promotion's allowlist | `src/fsguard.rs:357` (`git_restores`); `src/ops.rs:512` | `round5::r5_05_*` (3) |
| R5-06 | small: unique target names | `src/stack.rs:171` | `round5::r5_06_*` |
| R6-01 | A: stack edits below `Root::stack` | `src/ops.rs:87` (`stack_place`), `:101` (`layer_parts`) | `round6::r6_01_*` |
| R6-02 | A: one `Root::checkout` for all | `src/sync.rs:1184`; `src/fsguard.rs:751` | `round6::r6_02_*` (2) |
| R6-03 | E: the runner runs the lint | `tools/commit-check.sh:10` | `fsguard_lint::r6_03_*` |
| R6-04 | E: `r#` normalised | `tests/fsguard_lint.rs:187` (`unraw`) | `fsguard_lint::r6_04_*` |
| R6-05 | B: the `Complete` witness | `src/sync.rs:106`, `:1719`, `:2049` | `round6::r6_05_*`, `round6::b_*` |
| R6-06 | C: ARCHIVE or NORMAL only | `src/fsguard.rs:357`; `src/ops.rs:512` | `round6::r6_06_*` |
| R6-07 | D: the probe | `src/hooks.rs:43` (`probe_lines`), `:95` (`privacy_dispatch`) | `round6::r6_07_*`; `crates/lords/tests/cli.rs` (2) |
| R6-08 | D: any git error is not verified | `src/check.rs:147` (`privacy_hooks`) | `round6::r6_08_*` |
| Review point 1 | D: a nonce with its file | `src/hooks.rs:43` | `cli::an_invented_probe_value_*`, `cli::a_probe_with_its_nonce_file_*` |
| Review point 2 | A: set-aside within verified roots | `src/sync.rs:1894` (`containing`), `:2281` | `round6_api::set_aside_*` |
| Review point 3 | A: restore re-derives or refuses | `src/sync.rs:2112` | `round6_api::restore_refuses_*` |
| Rule A's acceptance | a plain path doesn't compile | `src/fsguard.rs:950` (doctest) | `cargo test --doc` |
| Staged first use | the install stage | `src/sync.rs:1745` (`staged`) | `round6_api::the_install_stage_*` |

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
