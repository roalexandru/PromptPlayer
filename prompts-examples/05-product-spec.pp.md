---
name: Product spec from one-liner
description: Large prompt — sales-engineer cadence, mixes expressions, variables, tab stops, choice
triggers: [spec, prd]
commit-char: ">"
priority: 75
typing-profile: sales-engineer
tags: [product, writing]
---
 me a v0 product spec for the following feature.

Feature one-liner:
$SELECTION

Audience: ${1|engineers,designers,product-managers,executives|}
Stage: ${2:exploration}
Target ship: ${{ (() => { const d = new Date(); d.setDate(d.getDate() + 28); return d.toISOString().split("T")[0]; })() }}

Use this skeleton, fill each section with the minimum that's actually decision-relevant:

## Problem
What hurts today and for whom. Quantify if you can.

## Goals (and explicit non-goals)
Three goals max. Three non-goals to bound the scope.

## Proposed solution
The one approach you'd recommend, plus the next-best alternative and why you didn't pick it.

## Risks
Top three risks ranked by impact. For each, a kill criterion or de-risker.

## Open questions
Things we'd need answered before starting. Tag each with who can answer.

---
Drafted by $USER on $DATE for ${APP_NAME}. Random sanity check: ${{ random_choice(["✅ ship if leadership says go", "✅ build a prototype first", "⚠️ this needs more discovery"]) }}
$0
