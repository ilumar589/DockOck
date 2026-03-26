# Dotnet-only Overlay

Use this overlay when the new repository is primarily a .NET backend or service codebase with little or no React frontend surface.

What it does:

- narrows task routing toward backend, architecture, API, database, build, review, and reliability agents
- keeps frontend agents available for manual invocation if needed, but removes them from automatic task delegation
- expands safe bash allowlists for common `dotnet` workflows

Apply by merging this folder's `opencode.json` into the repository root `opencode.json`.