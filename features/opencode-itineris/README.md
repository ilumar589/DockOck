# Itineris OpenCode Feature

This folder contains the full traceable footprint for the Itineris OpenCode customization feature added to this repository.

If you want to install this workflow into a new project, this is the best starting point because it shows the source layout, the reusable starter, and the recommended command order.

## Layout

- `.opencode/`: traceable source copy of the root runtime agent and command files
- `opencode.json`: traceable source copy of the root runtime OpenCode policy
- `starter/`: reusable starter package for new Itineris repositories
- `template-repo/`: publishable template-style package
- `scripts/`: maintenance helpers for syncing or exporting the feature assets

## What to copy into a new project

At minimum, a new project needs these items in its repository root:

1. `.opencode/`
2. `opencode.json`

Optional additions:

1. `custom_providers.json` if your team uses a shared provider catalog
2. `PROVIDER_SETUP.md` if you want local provider onboarding notes in the new project
3. a variant overlay from `starter/variants/` if the repository is strongly biased toward backend, frontend, or platform work

## Fastest setup path for a new project

### Option 1: use the starter directly

1. Copy `starter/.opencode/` to the new repository root as `.opencode/`.
2. Copy `starter/opencode.json` to the new repository root.
3. If needed, copy `starter/PROVIDER_SETUP.md` and your provider catalog file.
4. If the project is stack-specific, merge one overlay from `starter/variants/` into `opencode.json`.
5. Adjust the command allowlist in `opencode.json` so it matches the real build, test, and run commands of the new repo.
6. Run `/tech-stack-scan` in the new project so the pack can confirm the effective stack and route later work correctly.

### Option 2: use the bootstrap script

Run the bootstrap helper from this repository:

```powershell
features/opencode-itineris/starter/scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant none
```

Useful variants:

```powershell
features/opencode-itineris/starter/scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant dotnet-only
features/opencode-itineris/starter/scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant frontend-heavy
features/opencode-itineris/starter/scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant platform-heavy
```

If you also want provider onboarding notes copied into the target repo:

```powershell
features/opencode-itineris/starter/scripts/bootstrap-opencode.ps1 -TargetRepo <path> -Variant none -IncludeProviderNotes
```

## Setup checklist for the new project

After the files are copied, do this in order:

1. Confirm `.opencode/` and `opencode.json` are at the repository root.
2. Open `opencode.json` and update allowed shell commands so they match the new stack.
3. Remove commands the new repository should not run and add only the commands the team actually needs.
4. If your team uses a provider catalog, add `custom_providers.json` and verify the provider IDs and model IDs.
5. Read `.opencode/README.md` in the target repo to confirm the available agents and workflow commands.
6. Run `/tech-stack-scan` to confirm the effective stack and inferred defaults.
7. Run a small planning prompt first to verify the pack is discovered correctly.

## Recommended command order

Use the commands in this order for normal delivery work.

### 1. Optional stack confirmation

Use `/tech-stack-scan` first when the repository is new, partially scaffolded, or the effective stack is not fully obvious from the files yet.

Use it when:

- the project has just been bootstrapped
- the intended stack was documented separately from the codebase
- agent routing should be aligned before planning
- you want confirmed stack facts separated from fallback defaults

### 2. Optional architecture reconstruction

Use `/doc-mcp-architecture-scan` first when the architecture is mostly described in indexed documents and the team needs a synthesized view before planning implementation.

Use it when:

- the project is document-heavy
- system boundaries are unclear
- you need architecture detail from the Doc MCP corpus
- the PM coordinator needs evidence before slicing the work

### 3. Slice planning

Use `/plan-slice` to turn the request into one bounded delivery slice.

This is the normal entry point once the problem is clear enough to plan.

### 4. Implementation

Use `/implement-slice` after the slice is approved and scoped.

This should have one clear implementation owner and only the specialists that are actually needed.

### 5. Review

Use `/review-slice` when the implementation is complete and you want a read-only findings-first review.

### 6. Repair

Use `/repair-slice` if review finds defects or the implementation needs a focused correction pass.

### 7. Prompt and routing maintenance

Use `/improve-agents` only when the recurring problem is in the workflow layer itself, such as poor agent routing, weak prompts, or repeated coordination mistakes.

## Typical workflow patterns

### Standard feature flow

1. `/plan-slice`
2. `/implement-slice`
3. `/review-slice`
4. `/repair-slice` if needed

### New project or uncertain-stack flow

1. `/tech-stack-scan`
2. `/plan-slice`
3. `/implement-slice`
4. `/review-slice`
5. `/repair-slice` if needed

### Document-led architecture flow

1. `/tech-stack-scan`
2. `/doc-mcp-architecture-scan`
3. `/plan-slice`
4. `/implement-slice`
5. `/review-slice`
6. `/repair-slice` if needed

### Workflow tuning flow

1. deliver a few slices normally
2. notice a repeated routing or prompt problem
3. use `/improve-agents`

## Which package to use

- Use `starter/` when you want to install this workflow layer into an existing repository.
- Use `template-repo/` when you want to publish or clone a standalone internal template repository.
- Use this folder's `.opencode/` and `opencode.json` as the traceable source when maintaining the feature in DockOck.

## Operational note for this repository

OpenCode still requires `.opencode/` and `opencode.json` at the repository root for discovery.

In DockOck:

- the root files are operational copies
- this folder holds the traceable source copy
- use `scripts/sync-live-to-root.ps1` after editing this folder's `.opencode/` or `opencode.json` if you want to refresh the root runtime files from this feature folder

## Recommended maintenance workflow

1. Make feature-level documentation or packaging changes here first.
2. If runtime behavior changes, update this folder's `.opencode/` or `opencode.json` and sync to the root.
3. Keep `starter/` and `template-repo/` aligned when policy or agent behavior changes.