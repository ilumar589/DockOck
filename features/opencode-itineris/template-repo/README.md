# Itineris OpenCode Template Repo

This folder is structured as a standalone internal template repository for new Itineris projects.

Purpose:

- provide a copy-pasteable or publishable repository skeleton for the Itineris OpenCode workflow layer
- let teams start with a known-good agent pack, command set, and OpenCode policy without mining another project for files

Suggested repository contents:

- `.opencode/agents`: reusable Itineris agents
- `.opencode/commands`: slice workflow commands and maintenance commands
- `opencode.json`: project policy and routing defaults
- `scripts/bootstrap-opencode.ps1`: helper to install the template into a target repository
- `PROVIDER_SETUP.md`: provider catalog guidance for environments that use `custom_providers.json`
- `variants/`: stack-specific overlay configs

How to publish it internally:

1. Copy the contents of the sibling `starter` folder into this repository root, or use that folder as the initial source for this template repository.
2. Add your internal onboarding docs, preferred build commands, and provider setup notes.
3. Keep the starter generic and move repo-specific rules back into the consuming project.
4. Version prompt and policy changes so teams can adopt them deliberately.

This folder is stored under `features/opencode-itineris` so the whole feature can be traced in one place inside this repository.

Bootstrap usage:

- Run `scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant none`
- Use `-Variant dotnet-only`, `frontend-heavy`, or `platform-heavy` to copy a matching overlay file into the target repo
- Use `-IncludeProviderNotes` to copy provider guidance next to the installed config

Recommended next additions if you turn this into a real repo:

- a minimal `CONTRIBUTING.md` for agent update rules
- sample bootstrap instructions for .NET, frontend, and platform-heavy teams
- an internal changelog for prompt and policy updates