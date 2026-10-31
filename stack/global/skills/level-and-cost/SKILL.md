---
name: level-and-cost
description: Use whenever you design, change or run something that repeats or spends shared resources - a job, gate, check, proof set, pipeline, hook, brief or loop - and at every introspection point (a turn end, a hand-over, a daily check). Puts the work at its owner and level, keeps it modular, and makes its cost a measured, owned number, your own tokens and turns included.
---

# Level and cost

Waste is a defect, at every level: machines, builds, checks, agent time, tokens, the context every agent carries.

1. **Owner and level.** Say who owns this and at which layer it belongs. Not yours, or not this layer: raise it with
   its owner, with evidence. Never work around it beside the owner's code.
2. **Functional and modular.** Each part does one job behind a clear contract, so it can be replaced on its own. Find
   what already does this (tool, kind, template, rule) and extend it: one tool per stage (rule.one-tool-per-stage).
3. **Price it before you run it.** Runs × duration × cores and memory, and the decision it buys. Run the smallest check
   that can change that decision; the full suite once, at the gate that needs it.
4. **Declare from measurement.** Estimates, asks and frequencies come from the last measured runs (about ×1.5), not
   from guesses. A guess is marked as one and replaced by the first measurement.
5. **Measure yourself.** At each introspection point, compare your own numbers with what you declared and with last
   time: waits, reruns, duplicates, over-asks, tokens, turns, tool calls. Off by more than 2×: fix it at its source
   (rule.fix-recurring-mistakes-mechanically), not run by run.
6. **Question your own design.** Before hand-over ask: would I build it this way again? What would make it
   unnecessary? The best fix is often that the problem never arises. Put the answer in one line of the report, beside
   the owner-and-level verdict.

The project's own rules take precedence over this skill.
