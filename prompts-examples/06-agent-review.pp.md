---
name: Review the working tree
description: Agent-companion example — repo context, a choice, terminal-safe line breaks
triggers: [review, rev]
commit-char: ">"
typing-profile: fast-presenter
newline-mode: backslash-enter
tags: [agent, review, demo]
enabled: false
---
 the uncommitted changes on $GIT_BRANCH in $REPO_NAME.

Focus on ${1|correctness,readability,performance|} above everything else.

For each finding, give me:
- the file and line
- what breaks, concretely
- the smallest fix that addresses it

Skip anything you can't point at in the diff. If the change looks fine, say so plainly rather than inventing work.
