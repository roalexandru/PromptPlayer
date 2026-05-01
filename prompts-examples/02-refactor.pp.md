---
name: Refactor selected code
description: Medium prompt — uses $SELECTION variable + choice tab stop
triggers: [refactor, refac, rfc]
commit-char: ">"
priority: 100
typing-profile: sales-engineer
typing-overrides:
  iki-median-ms: 130
  typo-rate: 0.005
tags: [refactor, code]
---
 this code into ${1|async/await,Result-returning,iterator-based|} form.

Selected code:
$SELECTION

Style preferences:
- prefer guard clauses over nested ifs
- name intermediate variables when the expression is non-obvious
- keep the public signature unchanged

Filename context: ${TM_FILENAME}

Return only the refactored code, no commentary.
$0
