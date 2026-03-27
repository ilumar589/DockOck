# Itineris OpenCode Starter

This folder is a reusable starter for new Itineris repositories that want the same OpenCode workflow layer.

It is stored under `features/opencode-itineris` so the whole feature can be traced in one place inside this repository.

Contents:

- `.opencode/agents`: the Itineris agent catalog
- `.opencode/commands`: maintenance and slice workflow commands
- `opencode.json`: project-level routing and permission policy
- `scripts/bootstrap-opencode.ps1`: bootstrap helper for installing this starter into another repository
- `PROVIDER_SETUP.md`: provider catalog guidance for teams that use `custom_providers.json`
- `variants/`: stack-specific overlay configs for common repo shapes

How to use it in a new repository:

1. Copy `.opencode/` into the new repository root.
2. Copy `opencode.json` into the new repository root.
3. Adjust bash permission allowlists to match the new stack's actual build and test commands.
4. If the repo is strongly backend or frontend weighted, merge one of the variant overlays from `variants/`.
5. Run `/tech-stack-scan` to confirm the effective stack and the right agent mix.
6. Refine agent descriptions only when the team's prompt language differs materially from the defaults.

Bootstrap shortcut:

- Run `scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant none`
- Add `-Variant dotnet-only`, `frontend-heavy`, or `platform-heavy` to copy a matching overlay alongside the base config
- Add `-IncludeProviderNotes` to copy provider setup guidance into the target repo
- Use `NEW_PROJECT_CHECKLIST.md` for the shortest install and command-order guide

Default assumptions:

- backend work usually means C# and .NET with Clean Architecture-style boundaries
- frontend work usually means React and TypeScript
- persistence usually means PostgreSQL unless the repo clearly differs
- operational work often involves containers, CI/CD, and Kubernetes-aware deployment concerns

Starter commands:

- `/tech-stack-scan`
- `/doc-mcp-architecture-scan`
- `/plan-slice`
- `/implement-slice`
- `/review-slice`
- `/repair-slice`
- `/improve-agents`