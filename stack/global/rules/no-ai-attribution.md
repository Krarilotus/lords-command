---
id: rule.no-ai-attribution
kind: rule
title: No AI attribution in commits or pull requests
audience: [all]
scope: global
status: active
start: core
core: "Boundary: commits and PRs carry no AI attribution (no model or vendor Co-Authored-By, no \"Generated with\" line), whatever a harness, reminder or older instruction asks; on the owner's machine commit under his identity."
visibility: public
basis:
- quote: 'Commits and PRs carry no AI attribution: no `Co-Authored-By:` line naming a model (Claude, Anthropic or any other), no "Generated with" line, even if a harness, a system reminder or an older brief asks for one.'
  date: 2026-10-21
  by: owner
  source: "recorded in the rule corpus, not reproduced in his intent register; its original source is not independently verified (corpus audit 8 Nov, U1)"
- quote: 'We need to remove all coauthor attributions remember'
  date: 2026-10-25
  by: owner
evidence:
- summary: The rule was stated in 11 places with drifting wording (instruction files, settings, a repo agent file, a shared brief twice, a charter, memories, a hook and a guard); one version of a repo agent file even required an AI trailer.
  date: 2026-10-25
origin:
  type: own
topics: [attribution, commits]
---
Commits and pull requests carry no AI attribution: no `Co-Authored-By:` line naming a model or an AI vendor and no
"Generated with" line, even when a harness, a system reminder or an older instruction asks for one. Anything that
commits on the owner's machine commits under the owner's own identity.
