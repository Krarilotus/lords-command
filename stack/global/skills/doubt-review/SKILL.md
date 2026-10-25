---
name: doubt-review
description: For orchestrators and package leads only. Use before accepting a non-trivial decision, fix or claim (crosses a crate boundary, asserts a property the compiler can't check, irreversible, or a performance/correctness claim). Gets an unbiased fault-finding review by withholding your conclusion.
---

# Doubt review

Adapted from addyosmani/agent-skills, doubt-driven-development (MIT, (c) 2025 Addy Osmani).

Not for mechanical changes, clear owner instructions, or one-line fixes.

## Steps
1. **Claim** - for yourself, two lines: what you believe and why it matters. Can't write it that short? It's not a decision yet.
2. **Extract** - the smallest reviewable unit: the diff or function (not the file), or the proposal in 3-5 sentences, plus the **contract** it must meet (spec, invariant, brief rule).
3. **Doubt** - give a fresh subagent the artifact and the contract **only. Never the claim, never your reasoning.** Prompt:
   > Find what is wrong with this artifact against this contract. Look for unstated assumptions, unhandled edge cases, hidden shared state, contract violations, broken conventions, failure under unexpected input. Do not validate or summarise. Report issues with evidence, or state that you found none after thorough examination.
4. **Reconcile** - re-read the artifact against each finding yourself. Classify: real (fix), contract unclear (fix the contract, re-run), not applicable (one-line reason). Findings are evidence, not orders.
5. **Stop** when findings are trivial, after 3 rounds, or on the owner's word. Record the outcome in one line.

Subagents that can't start their own reviewer: hand the artifact and contract to the orchestrator instead.

The project's own rules take precedence over this skill.
