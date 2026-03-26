# Platform-heavy Overlay

Use this overlay when the new repository is dominated by infrastructure, CI/CD, containers, deployment automation, runtime configuration, or Kubernetes operations.

What it does:

- biases automatic task delegation toward DevOps, Kubernetes, SRE, build, security, performance, and documentation flows
- keeps application-focused agents available for manual invocation, but removes most of them from automatic task delegation
- expands safe bash allowlists for build, container, and deployment-oriented workflows while keeping destructive operations gated

Apply by merging this folder's `opencode.json` into the repository root `opencode.json`.