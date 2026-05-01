---
name: Strategic memo to leadership
description: Large prompt — Thoughtful CEO cadence, expressions for date + framing variation
triggers: [memo, strategy, brief]
commit-char: ">"
priority: 0
typing-profile: thoughtful-ceo
tags: [exec, writing, strategy]
---
 me draft a memo to the leadership team, dated $DATE.

Subject: ${{ random_choice(["Quarterly review and what's next", "Strategic priorities update", "Course correction proposal"]) }}

Frame the message around three pillars:
1. What we shipped this quarter and what it cost us — be specific, name the trade-offs we made consciously.
2. Where the market is moving and what that means for our roadmap. Don't hedge.
3. What I'm asking the team for next quarter. Single biggest ask, then secondary asks.

Tone: direct, executive, no jargon. Short paragraphs. End with one sentence on what good looks like in 90 days.

Today is ${{ now.toISOString().split("T")[0] }}. Reference the prior quarter explicitly.

Length: ~600 words. Markdown headers for each pillar.
