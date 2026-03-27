# New Project Checklist

Use this when you want the fastest possible setup for a new repository.

## Install

1. Copy `.opencode/` to the new repository root.
2. Copy `opencode.json` to the new repository root.
3. If your team uses provider catalogs, copy `custom_providers.json` and optionally `PROVIDER_SETUP.md`.
4. If the repository is stack-heavy, merge one matching overlay from `variants/`.

## Configure

1. Open `opencode.json`.
2. Keep only the shell commands the new repository should actually run.
3. Add the real build, test, run, and package commands for that stack.
4. Run `/tech-stack-scan` so the pack can confirm the effective stack and inferred defaults.
5. Verify agent descriptions still match how your team asks for work.

## Verify discovery

1. Confirm `.opencode/` and `opencode.json` are at the repository root.
2. Open the project in OpenCode.
3. Check that the Itineris agents and commands are discovered.
4. Use `/tech-stack-scan` as the first read-only verification command.

## Use the commands in this order

1. `/tech-stack-scan` when the repository is new or the effective stack should be confirmed.
2. `/doc-mcp-architecture-scan` when architecture must be reconstructed from indexed documents.
3. `/plan-slice` to define one bounded slice.
4. `/implement-slice` to execute that slice.
5. `/review-slice` for findings-first review.
6. `/repair-slice` if review finds issues.
7. `/improve-agents` only when the workflow prompts or routing need tuning.

Use `@database-testability-engineer` during implementation when the slice needs seed data, repeatable inserts, or reliable DB-backed test setup.

## Default flow

1. Start with `/tech-stack-scan` when the project is new or the stack is not obvious.
2. Start with `/plan-slice` for normal feature work once the stack is clear.
3. Add `/doc-mcp-architecture-scan` before planning when the project is document-led.
4. Keep one implementation owner per slice.
5. Review before widening scope.