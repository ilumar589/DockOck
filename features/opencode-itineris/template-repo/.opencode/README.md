# Itineris OpenCode Agent Pack

This project now includes a project-local OpenCode agent pack in `.opencode/agents`, a maintenance command in `.opencode/commands`, and a project policy file in `opencode.json`.

OpenCode discovers these markdown files automatically. The filename becomes the agent name, so examples include `@describer`, `@business-analyst`, `@backend-developer`, and `@qa-engineer`.

Recommended starting combinations from the AI workflow playbook:

- Planning: `@describer`, `@business-analyst`, `@principal-technical-expert`, `@software-architect`
- Implementation: `@backend-developer` or `@frontend-developer`, with `@project-preferences-advisor` attached
- Review: `@backend-code-reviewer` or `@frontend-code-reviewer`, plus `@qa-engineer`
- Repair: return to the original implementation owner with `@qa-engineer`, then add specialists by failure mode
- Specialists: `@api-designer`, `@architecture-advisor`, `@database-architect`, `@ddia-advisor`, `@ui-ux-designer`, `@build-engineer`, `@devops-engineer`, `@kubernetes-expert`, `@kafka-expert`, `@redis-expert`, `@test-automation-engineer`, `@performance-engineer`, `@security-engineer`, `@sre-engineer`, `@documentation-expert`, `@product-engineer`, `@pm-coordinator`

Stack assumptions baked into the prompts:

- backend defaults to C# and .NET with Clean Architecture-style boundaries
- frontend defaults to React and TypeScript
- persistence defaults to PostgreSQL unless the repository clearly uses something else
- operational concerns assume containerized deployment and often Kubernetes-backed runtime environments

Project policy highlights from `opencode.json`:

- `build` remains the default primary agent for execution
- `plan` is kept read-only and can only delegate to planning and review-oriented subagents
- `build` can delegate to the Itineris agents and use common safe commands without reprompting every time
- broad destructive shell behavior remains gated behind approval

Maintenance:

- Use `/improve-agents` when the same workflow mistakes recur across multiple slices and the problem looks like weak prompts or weak routing rather than a one-off defect

Notes:

- The agents do not pin a model, so they will use the active OpenCode model and provider configuration for the project.
- Planning and review agents are read-only by default through `permission.edit: deny`.
- If you want OpenCode to auto-route more aggressively, keep the `description` fields specific and aligned to the language your team actually uses in prompts.