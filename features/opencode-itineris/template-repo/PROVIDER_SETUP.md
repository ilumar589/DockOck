# Provider Setup

This template repo expects teams to separate workflow behavior from provider catalog management.

Use this structure in a consuming repository:

- `custom_providers.json` for provider and model catalog definitions
- `opencode.json` for project routing, permissions, and default agent behavior
- `.opencode/` for agents and commands

If your Itineris environment uses the same provider scheme as DockOck, keep the `bytedance` provider identifier and compatible model IDs stable across repositories so prompts and team guidance transfer cleanly.

Provider catalog tips:

- keep generator, reviewer, extractor, and vision defaults documented in the repo README
- prefer updating the shared provider catalog over hard-coding provider details into agent prompts
- verify new model IDs before updating team-wide starter templates