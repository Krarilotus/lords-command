# Third-party notices

Lords Command is MIT-licensed (see `LICENSE`). The public stack also ships skills that come from other projects.
Each one keeps its source, commit and licence in its `lords.yaml`, and its licence text beside it.

## Adapted skills

Rewritten from the source; the `changes` field of each `lords.yaml` says how.

| Skill | Source | Commit | Licence | Copyright |
|---|---|---|---|---|
| `stack/global/skills/scope-discipline` | [addyosmani/agent-skills](https://github.com/addyosmani/agent-skills) `skills/incremental-implementation/SKILL.md` | `aeac41d4d005999432bb7312f7ff06383a91d59d` | MIT (`LICENSE` beside it) | Copyright (c) 2025 Addy Osmani |
| `stack/global/skills/stop-the-line` | addyosmani/agent-skills `skills/debugging-and-error-recovery/SKILL.md` | `aeac41d4d005999432bb7312f7ff06383a91d59d` | MIT (`LICENSE` beside it) | Copyright (c) 2025 Addy Osmani |
| `stack/global/skills/spec-tests` | addyosmani/agent-skills `skills/test-driven-development/SKILL.md` | `aeac41d4d005999432bb7312f7ff06383a91d59d` | MIT (`LICENSE` beside it) | Copyright (c) 2025 Addy Osmani |
| `stack/global/skills/doubt-review` | addyosmani/agent-skills `skills/doubt-driven-development/SKILL.md` | `aeac41d4d005999432bb7312f7ff06383a91d59d` | MIT (`LICENSE` beside it) | Copyright (c) 2025 Addy Osmani |

## Inherited skills

Kept byte for byte; `lords check` verifies the recorded hash.

| Skill | Source | Commit | Licence | Copyright |
|---|---|---|---|---|
| `stack/global/skills/frontend-design` | [anthropics/skills](https://github.com/anthropics/skills) `skills/frontend-design` | `41bbe19d1a1a7eaab5e7bb9050a417e5c6cffc8f` | Apache-2.0 (`LICENSE.txt` beside it) | Anthropic; the licence file carries no copyright line |
