---
description: "Use when recurring workflow failures suggest agent prompts, descriptions, or routing rules should be tightened instead of rediscovered on every slice."
agent: plan
---
Review the recent workflow friction in this project and determine whether the root cause is an agent or prompt quality issue.

Your task:

1. Identify repeated planning, implementation, review, or repair failures.
2. Distinguish one-off defects from recurring prompt or routing problems.
3. Recommend precise updates to the relevant OpenCode agent files, descriptions, or permissions.
4. Flag duplicate, stale, or weak instructions that should be removed or consolidated.
5. Do not edit files automatically unless the user explicitly asks for prompt updates in this run.

Return:

- the repeated failure pattern
- the agent or command files that should change
- the exact behavioral gap
- the smallest prompt or policy update that would fix it