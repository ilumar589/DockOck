---
description: "Use when a completed slice needs structured review for correctness, risk, and test adequacy before acceptance."
agent: plan
---
Review the current slice using the Itineris workflow.

Your job:

1. Choose the right primary review owner: `@backend-code-reviewer` or `@frontend-code-reviewer`.
2. Attach `@qa-engineer` by default.
3. Add `@security-engineer`, `@performance-engineer`, `@sre-engineer`, `@architecture-advisor`, `@documentation-expert`, or `@test-automation-engineer` only if the risk profile justifies them.
4. Keep the review read-only and findings-first.
5. Distinguish confirmed defects from open questions and residual risks.

Return:

- findings ordered by severity
- missing tests or validation gaps
- specialist review additions that were warranted
- acceptance assessment and repair recommendation