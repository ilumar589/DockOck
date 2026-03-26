---
description: "Use when a reviewed slice has defects or validation gaps and should be repaired by returning to the original implementation owner with QA context."
agent: build
---
Repair the current slice using the Itineris workflow.

Your job:

1. Return to the original implementation owner when that is clear.
2. Attach `@qa-engineer` and any specialist that matches the failure mode.
3. Fix the narrowest root cause first.
4. Re-run the most relevant focused validation.
5. Keep repair bounded; if the failure is actually a planning defect, stop and route back to planning.

Return:

- defect or gap repaired
- files changed
- validation rerun
- remaining risks or reasons to return to planning