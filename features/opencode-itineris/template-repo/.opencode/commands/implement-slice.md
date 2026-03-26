---
description: "Use when a bounded slice is approved and should be implemented with one clear code-owning agent plus the right specialist support."
agent: build
---
Implement the current bounded slice using the Itineris workflow.

Your job:

1. Confirm the slice and choose one primary implementation owner: `@backend-developer` or `@frontend-developer`.
2. Attach `@project-preferences-advisor` and any essential specialists only if the slice clearly needs them.
3. Inspect existing repository patterns before editing code.
4. Make the smallest production-quality change set that satisfies the slice.
5. Run focused validation and summarize residual risks.

Constraints:

- Do not widen scope.
- Do not run review mode as the primary implementation pass.
- Prefer one clear owner over parallel implementation agents.

Return:

- implemented scope
- files changed
- validation performed
- residual risks or follow-up work