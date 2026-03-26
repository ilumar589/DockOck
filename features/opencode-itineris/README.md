# Itineris OpenCode Feature

This folder contains the full traceable footprint for the Itineris OpenCode customization feature added to this repository.

Layout:

- `.opencode/`: traceable source copy of the root runtime agent and command files
- `opencode.json`: traceable source copy of the root runtime OpenCode policy
- `starter/`: reusable starter package for new Itineris repositories
- `template-repo/`: publishable template-style package
- `scripts/`: maintenance helpers for syncing or exporting the feature assets

Operational note:

- OpenCode still requires `.opencode/` and `opencode.json` at the repository root for discovery
- those root files are operational copies
- use `scripts/sync-live-to-root.ps1` after editing this folder's `.opencode/` or `opencode.json` if you want to refresh the root runtime files from this feature folder

Recommended maintenance workflow:

1. Make feature-level documentation or packaging changes here first.
2. If runtime behavior changes, update this folder's `.opencode/` or `opencode.json` and sync to the root.
3. Keep `starter/` and `template-repo/` aligned when policy or agent behavior changes.