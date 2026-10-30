---
id: intent.organisation-principles
kind: intent
title: How the owner wants work organised, above any single release
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
visibility: public
basis:
- quote: "Our own design … I don't have to adapt my structure to someone else's artificial boundaries."
  date: 2026-10-25
  by: owner
- quote: "if that takes a bit, sure, we should do it properly."
  date: 2026-10-25
  by: owner
- quote: "fragmented, split-brain organisations \"die over time\""
  date: 2026-10-25
  by: owner
- quote: "we still should strive for the optimized performance at every level"
  date: 2026-10-25
  by: owner
- quote: "Being mindful of our ecosystem is an important rule we can't go without … if we are building the backbone of world memory, that is a priority at the core."
  date: 2026-10-25
  by: owner
- quote: "I made the wrong decision to push first for v1.5 completion but I misjudged how far out it was. It would be better to always take a step back … assess where you stand, and make sure you can actually reach the goals … and scale beyond them."
  date: 2026-10-25
  by: owner
- quote: "we need to sort through all the rules and leftovers anyways and make sure we confirm with me what isn't clear but also clean up obvious bad apples from the past."
  date: 2026-10-25
  by: owner
- quote: "Remove the specifics in brackets; such rules are not made to be overspecific; it needs to be enough freedom for the judge to understand and interpret at each moment."
  date: 2026-10-21
  by: owner
- quote: "We should always assess the ownership and abstraction levels for each decision because it could reduce the overall infrastructure and decisions we need to make overall and strengthens the process"
  date: 2026-10-21
  by: owner
- quote: "its easier to replace a separated module or component rather than some hardstuck stuff in the main code base, and that should be the philosophy anyways for many other parts as well to be modular flexible and scaleable by design"
  date: 2026-10-20
  by: owner
- quote: "the hierarchical orchestration exists for a reason, and that is not to micromanage and give a bit of agency to each one, so they can also freely question the design and actually give good feedback on thigns that otherwise get missed"
  date: 2026-10-26
  by: owner
  source: "principle 3, \"Don't micromanage\""
- quote: "the unified base for all agents regardless which provider, and only agent specific behavior things that only work for specific models and are necessary to get their behavior adjusted there in codex or claude specific storage"
  date: 2026-10-30
  by: owner
  source: "principle 4, on Codex Desktop importing Claude skills into .agents/"
evidence:
- summary: Disk overflow from leaked per-process test stores and caches "came up multiple times"; resource lifecycle by mechanism was asked for instead of sweeps
  date: 2026-10-25
- summary: Design rounds failed when briefs were mostly bans and duties; intents that lived only in chat or were paraphrased were not honoured, those written early into shared rules were
  date: 2026-10-05
origin:
  type: own
topics: [process, design, performance, memory, cleanup, owner-decisions]
---
These goals rank above any single release.

1. **One source of truth** for rules, intent, status and memory. Each rule lives once, in one place, short; briefs and
   messages point to it instead of restating it.
2. **A modular design**, carried over to more projects and improved over time. Prefer replaceable components behind
   clear contracts over code stuck inside one codebase.
3. **A strict, consistent rule corpus that scales.** Rules state a principle and its reason and leave room for
   judgement; concrete cases are evidence, not rule text. Don't micromanage: pass findings as facts and let agents
   judge. What the owner's intent can't settle goes to the owner; obvious bad apples are cleaned up.
4. **No vendor lock-in.** Our own design: memory and rules work for every agent family, and other harnesses are
   evidence, not templates. Lords Command is the unified base for all agents, whatever the provider; only behaviour
   that is specific to one model and needed to adjust it there lives in that provider's own storage.
5. **Own the codebase.** Intuitive navigation with clear ownership and abstraction levels, so the owner can take control
   and change things quickly. Meta-schemas for organisation live inside the codebase and are enforced. Fast, direct
   paths between layers, without artificial barriers. Standards are defined, visible, readable and adaptable.
6. **Owner and level first.** Before each decision, assess which ownership and abstraction level it belongs to, and
   fix it there.
7. **Scalability first**, even when doing it properly takes longer.
8. **Optimised performance at every level**, from the product to builds, checks, machines, agent time and tokens.
   Organisation never buys clarity with speed. Waste is a defect: being mindful of the ecosystem is a core rule.
   Tool technology (a UI renderer, say) is chosen by measurement, never by assumption. Structural moves (splits,
   renames, module moves) are timed where they could affect hot paths.
9. **Step back before pushing on.** Before any push toward a milestone, assess where things stand, check the goal can
   be reached and say what blocks it. Automate the process as much as the software.

How to apply:
- The same pattern everywhere: a schema, a check and generated views, living in the repo they govern.
- Resources are managed by mechanism, not by sweeps: registered at creation (owner, purpose, size, expiry), a
  free-space floor before admission, job-scoped temp roots, size caps on shared caches, only our own labelled
  resources touched, exceptions only by a human.
- The owner's intent is recorded early, in their own words, with stable ids; it is quoted, never paraphrased.
- Keep designs graph-shaped (stable ids, typed relations), without building beyond what is needed now.
