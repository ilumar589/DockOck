# Frontend-heavy Overlay

Use this overlay when the new repository is primarily a React and TypeScript application with limited backend ownership inside the repo.

What it does:

- biases automatic task delegation toward frontend, UX, API-consumer, test automation, and frontend review flows
- removes backend-only agents from automatic task delegation while keeping them available for manual invocation if needed
- expands safe bash allowlists for common frontend build and validation workflows

Apply by merging this folder's `opencode.json` into the repository root `opencode.json`.