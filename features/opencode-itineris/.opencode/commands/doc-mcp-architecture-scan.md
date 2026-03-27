---
description: "Use when the indexed Doc MCP corpus should be scanned to derive architecture detail, system boundaries, integration maps, and planning inputs before implementation starts."
agent: plan
---
Scan the Doc MCP corpus and derive an architecture view for this Itineris project.

Your job:

1. Use `@doc-mcp-architecture-coordinator` as the primary specialist for corpus-wide architecture extraction.
2. Pair `@pm-coordinator` so the architecture findings are translated into execution sequencing and slice guidance.
3. Review the available document corpus broadly before narrowing to individual documents.
4. Extract systems, boundaries, integrations, environments, data flows, constraints, and operational concerns that are supported by the documents.
5. Flag contradictions, missing evidence, and decisions that still require human confirmation.
6. Return architecture detail that is specific enough to guide subsequent `/plan-slice` and implementation work.

Return:

- corpus coverage reviewed
- architecture landscape summary
- systems, boundaries, and integrations
- data and control flows
- delivery coordination guidance
- gaps, contradictions, and open decisions