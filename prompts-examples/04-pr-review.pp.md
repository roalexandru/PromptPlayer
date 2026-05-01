---
name: PR review from clipboard
description: Medium prompt — clipboard-driven code review with tab stops
triggers: [review, pr]
commit-char: ">"
priority: 50
typing-profile: sales-engineer
tags: [review, code]
---
 this PR for me.

Diff (from clipboard):
$CLIPBOARD

What I want you to focus on:
1. ${1:correctness — are there latent bugs?}
2. ${2:security — any new attack surface?}
3. ${3:test coverage — what's missing?}

Skip nits about formatting or naming unless they hide a real bug. Don't restate what the diff does — only call out what concerns you.

Author: $USER
Reviewed at: $TIME on $DATE.
$0
