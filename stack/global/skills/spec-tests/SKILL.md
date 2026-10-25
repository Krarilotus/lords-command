---
name: spec-tests
description: Use whenever writing, changing or reviewing a test. Every test must state specified behaviour and fail when that behaviour breaks - no tautologies, no tests that restate the implementation.
---

# Spec tests

Adapted from addyosmani/agent-skills, test-driven-development (MIT, (c) 2025 Addy Osmani), reframed: the goal is tests of specified behaviour, not more tests.

## Before writing a test, write its spec line
`Spec: <source - standard section, issue, design note, owner decision> says <input/state> -> <observable result>.`
No source you can name? Then it is not yet a spec; ask, or state the behaviour the user relies on.

## The red check (required)
**The test must fail without the change and pass with it.** Record both outcomes in your report. A test that passes either way proves nothing; delete or rewrite it.
- **By default, commit the test before the fix.** Its failure on the pre-fix tree is the red evidence, and the fix commit shows it green. Where a fleet job or CI exists, let it run only the new or changed tests against the parent tree.
- **Mutation after the fact is the fallback** (undo the fix in a copy and rerun the test), only for a test written after its fix. Say in your report why it came after.

## A test is tautological - rewrite it - when
- the expected value is computed by the code under test, or copied from its output without an independent source;
- it asserts only that a function was called, a struct has the fields you just set, or a mock returns what you told it to;
- it checks the implementation's steps instead of the observable result (it breaks on a correct refactor);
- it duplicates another test with a different literal (fold into a table instead);
- the assertion was weakened until it passed.

## Good tests
- Expected values from the spec, a hand-worked example, or an independent reference.
- Edge cases the spec names (empty, boundary, error, concurrency), each once.
- For bugs: the smallest input that showed the bug.

## Never
Delete, `#[ignore]`/skip, or loosen an existing test to make a change pass. If an existing test looks wrong, say why with its spec line and let the orchestrator decide.

The project's own rules take precedence over this skill.
