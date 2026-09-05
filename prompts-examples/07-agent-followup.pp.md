---
name: Review, then ask for tests
description: Multi-step example — one message, a pause, then the follow-up
triggers: [reviewtests, rt]
commit-char: ">"
typing-profile: sales-engineer
newline-mode: backslash-enter
tags: [agent, demo, multi-step]
enabled: false
---
 the uncommitted changes on $GIT_BRANCH and tell me what looks risky.

Be specific: file, line, what breaks. Skip anything you can't point at.

<!-- pp:wait 45s -->

Now add tests for the riskiest thing you found.

One test per behaviour, and make each one fail first if the bug were reintroduced — I want to see the guard actually catch it.
