---
name: scope-discipline
description: Use whenever implementing or fixing code in an assigned package or task. Keeps changes to what the task needs, in thin verified slices; everything else is listed, not fixed.
---

# Scope discipline

Adapted from addyosmani/agent-skills, incremental-implementation (MIT, (c) 2025 Addy Osmani).

## Rules
1. **Before the first edit,** write in two lines: what the task changes, and what it explicitly does not touch.
2. **Touch only what the task needs.** No clean-ups of neighbouring code, no renames, import shuffles or "while I'm here" fixes, no features that "seem useful".
3. **Search before you write.** A helper, type or test fixture you need probably exists. Reuse it at its owning place instead of adding a near-copy.
4. **Don't grow big files** (over ~600 lines) unless that file is the owning place for the change.
5. **One slice at a time:** smallest change that compiles and can be checked -> check it -> commit -> next slice. Refactors and behaviour changes go in separate commits.
6. **Scope grows?** (more crates, a shared type, a design question) Stop and ask the orchestrator once, with a concrete proposal. Don't widen silently.

## Noticed but not touching
Everything you saw and didn't fix goes in your report, one line each:

```
NOTICED BUT NOT TOUCHING:
- <file:line> <what> (<why it matters>, owner if known)
```

The orchestrator decides whether it becomes a task.

The project's own rules take precedence over this skill.
