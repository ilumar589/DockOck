# Itineris OpenCode Agent Pack

This project now includes a project-local OpenCode agent pack in `.opencode/agents`, a maintenance command in `.opencode/commands`, and a project policy file in `opencode.json`.

The traceable source for this feature now lives under `features/opencode-itineris`. The root `.opencode/` folder and root `opencode.json` remain in place because OpenCode discovers them from the project root.

OpenCode discovers these markdown files automatically. The filename becomes the agent name, so examples include `@describer`, `@business-analyst`, `@backend-developer`, and `@qa-engineer`.

Recommended starting combinations from the AI workflow playbook:

- Planning: `@describer`, `@business-analyst`, `@principal-technical-expert`, `@tech-stack-advisor`, `@software-architect`
- Implementation: `@backend-developer` or `@frontend-developer`, with `@project-preferences-advisor` attached
- Review: `@backend-code-reviewer` or `@frontend-code-reviewer`, plus `@qa-engineer`
- Repair: return to the original implementation owner with `@qa-engineer`, then add specialists by failure mode
- Specialists: `@api-designer`, `@architecture-advisor`, `@doc-mcp-architecture-coordinator`, `@database-architect`, `@database-testability-engineer`, `@ddia-advisor`, `@tech-stack-advisor`, `@ui-ux-designer`, `@build-engineer`, `@devops-engineer`, `@kubernetes-expert`, `@kafka-expert`, `@redis-expert`, `@test-automation-engineer`, `@performance-engineer`, `@security-engineer`, `@sre-engineer`, `@documentation-expert`, `@product-engineer`, `@pm-coordinator`

Doc MCP coordination pattern:

- Pair `@doc-mcp-architecture-coordinator` with `@pm-coordinator` when the repository architecture must be reconstructed from the indexed document corpus before slices are planned or assigned.

Stack assumptions baked into the prompts:

- backend defaults to ASP.NET Core Minimal API on .NET 10 with C#, Clean Architecture, EF Core, MediatR, FluentValidation, and Serilog
- frontend defaults to React 19 with TypeScript 5, Vite 6, shadcn/ui, Tailwind CSS v4, Zustand, TanStack Query, React Hook Form with Zod, React Router 7, and Axios
- persistence defaults to PostgreSQL 16 with PostGIS, with Redis for distributed cache, Keycloak for identity, Azure Blob Storage for document storage, and Azure Cognitive Search for full-text search
- testing defaults to xUnit, Vitest, Playwright, and Testcontainers with strong coverage expectations on critical flows
- operational concerns assume Docker and Docker Compose locally, Azure DevOps Pipelines and Azure Bicep for delivery, and Azure Application Insights with Azure Log Analytics for observability

Project policy highlights from `opencode.json`:

- `build` remains the default primary agent for execution
- `plan` is kept read-only and can only delegate to planning and review-oriented subagents
- `build` can delegate to the Itineris agents and use common safe commands without reprompting every time
- common destructive shell patterns such as file deletion, hard reset, checkout-revert, and git push are explicitly denied
- `.env` reads are explicitly denied except for `.env.example` patterns
- the default safe command allowlist is now biased to this workspace's actual toolchain: `cargo`, `npm install`, `npm start`, `node server.js`, and docker compose commands

Maintenance:

- Use `/improve-agents` when the same workflow mistakes recur across multiple slices and the problem looks like weak prompts or weak routing rather than a one-off defect

Workflow commands:

- `/tech-stack-scan` identifies the effective repository stack, highlights inferred defaults, and recommends the right agent mix before planning or customization
- `/doc-mcp-architecture-scan` scans the indexed Doc MCP corpus with `@doc-mcp-architecture-coordinator` and `@pm-coordinator` to produce architecture detail and coordination input
- `/plan-slice` turns a request into a bounded slice with the right planning agents
- `/implement-slice` executes one approved slice with a single primary implementation owner
- `/review-slice` runs the findings-first review stack
- `/repair-slice` returns to the implementation owner with QA and specialist context

Reusable starter and variants:

- A reusable copy of this setup now lives in `features/opencode-itineris/starter`
- Apply `variants/dotnet-only/opencode.json` when a new repo is backend-dominant and should bias toward .NET delivery
- Apply `variants/frontend-heavy/opencode.json` when a new repo is frontend-dominant and should bias toward React and TypeScript delivery
- Apply `variants/platform-heavy/opencode.json` when a new repo is infrastructure, container, CI/CD, or Kubernetes heavy

Standalone template repo:

- A publishable template-style package now lives in `features/opencode-itineris/template-repo`
- Use it when you want to create a separate internal repository that teams can clone or copy from directly rather than pulling files out of this repo

Bootstrap and providers:

- `features/opencode-itineris/starter/scripts/bootstrap-opencode.ps1` installs the starter into a target repo and can apply a stack overlay
- `features/opencode-itineris/starter/PROVIDER_SETUP.md` explains how to align a new repo with `custom_providers.json`
- `features/opencode-itineris/scripts/sync-live-to-root.ps1` refreshes the root runtime files from the traceable feature folder

Notes:

- The agents do not pin a model, so they will use the active OpenCode model and provider configuration for the project.
- Planning and review agents are read-only by default through `permission.edit: deny`.
- If you want OpenCode to auto-route more aggressively, keep the `description` fields specific and aligned to the language your team actually uses in prompts.