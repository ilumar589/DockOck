# Provider Setup

This starter is designed to work with a project-local OpenCode configuration and can also align with an external provider catalog such as [custom_providers.json](../../../custom_providers.json).

Recommended approach for a new Itineris repository:

1. Keep the OpenCode workflow layer in `.opencode/` and `opencode.json`.
2. If your environment uses a shared provider catalog, place a `custom_providers.json` file in the new repository root.
3. Keep provider IDs and model IDs stable across repositories so agent defaults and team guidance stay predictable.
4. Document the intended generator, reviewer, extractor, and vision defaults for the team.

Current DockOck example:

- provider id: `bytedance`
- provider label: `rinf.tech AIArk`
- default generator: `deepseek-v3-2-251201`
- default reviewer: `kimi-k2-thinking-251104`
- default extractor: `seed-1-6-flash-250715`
- default vision: `seed-2-0-lite-260228`

Practical guidance:

- keep agent prompts model-agnostic unless a specific model capability is required
- if a new repo uses a smaller or faster model for planning, set that in `opencode.json` rather than hard-coding it into each agent file
- if your organization rotates provider endpoints or model IDs, update the provider catalog and leave the workflow layer mostly unchanged

Suggested team convention:

- repository root contains `custom_providers.json`
- repository root contains `opencode.json`
- `.opencode/` contains agents and commands
- the README explains which provider and model family the team expects for generation and review