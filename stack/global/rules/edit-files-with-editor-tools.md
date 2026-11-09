---
id: rule.edit-files-with-editor-tools
kind: rule
title: "Editing files"
audience: [all]
roles: [orchestrator, coder, helper, reviewer]
scope: global
status: active
start: detail
visibility: public
origin:
  type: own
topics: [process]
---
**Purpose:** the shell can mangle backslashes, encodings and line endings, and its edits are hard to review.

**Default:** write and edit files with the harness's editor tools, not through shell redirection.

**When to depart:** a mechanical change across many files, or a format the editor can't write safely, may use another
structured writer whose result you check (a reviewed diff); say which and why.
