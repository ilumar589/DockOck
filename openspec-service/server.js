// DockOck OpenSpec Service
//
// HTTP service that accepts Gherkin output from DockOck and produces
// OpenSpec change artifacts (proposal, specs, design, tasks).
//
// Endpoints:
//   GET  /health              — liveness check
//   POST /generate            — generate OpenSpec artifacts from Gherkin
//
// The service maintains an OpenSpec workspace in /workspace.

import { createServer } from "node:http";
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync, existsSync, rmSync } from "node:fs";
import { join } from "node:path";

const PORT = Number(process.env.PORT || 3000);
const WORKSPACE = process.env.WORKSPACE || "/workspace";
const OLLAMA_URL =
  process.env.OLLAMA_URL || "http://ollama-generator:11434";
const OLLAMA_MODEL = process.env.OLLAMA_MODEL || "qwen2.5-coder:32b";

// ─────────────────────────────────────────────
// OpenSpec workspace bootstrap
// ─────────────────────────────────────────────

function ensureWorkspace() {
  if (!existsSync(join(WORKSPACE, "openspec"))) {
    mkdirSync(WORKSPACE, { recursive: true });
    try {
      execFileSync("openspec", ["init", "--tools", "none"], {
        cwd: WORKSPACE,
        stdio: "pipe",
        timeout: 30_000,
      });
      console.log("[openspec] Workspace initialised at", WORKSPACE);
    } catch {
      // CLI may not have init in non-interactive mode — scaffold manually
      console.log("[openspec] CLI init failed, creating structure manually");
      mkdirSync(join(WORKSPACE, "openspec", "specs"), { recursive: true });
      mkdirSync(join(WORKSPACE, "openspec", "changes"), { recursive: true });
    }
  }
}

// ─────────────────────────────────────────────
// Gherkin → OpenSpec converters
// ─────────────────────────────────────────────

/** Parse raw Gherkin feature text into structured data. */
function parseGherkin(text) {
  const feature = { title: "Generated Feature", description: "", scenarios: [] };
  let currentScenario = null;

  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("Feature:")) {
      feature.title = line.slice("Feature:".length).trim();
    } else if (
      line.startsWith("Scenario Outline:") ||
      line.startsWith("Scenario:")
    ) {
      if (currentScenario) feature.scenarios.push(currentScenario);
      const isOutline = line.startsWith("Scenario Outline:");
      const prefix = isOutline ? "Scenario Outline:" : "Scenario:";
      currentScenario = {
        title: line.slice(prefix.length).trim(),
        steps: [],
        isOutline,
      };
    } else if (currentScenario) {
      const kw = ["Given", "When", "Then", "And", "But"].find((k) =>
        line.startsWith(k + " ")
      );
      if (kw) {
        currentScenario.steps.push({
          keyword: kw,
          text: line.slice(kw.length + 1).trim(),
        });
      }
    } else if (line && !line.startsWith("#") && feature.title !== "Generated Feature") {
      // Description lines between Feature: and first Scenario:
      feature.description += (feature.description ? "\n" : "") + line;
    }
  }
  if (currentScenario) feature.scenarios.push(currentScenario);
  return feature;
}

/** Convert parsed Gherkin into OpenSpec spec.md format. */
function toSpecMd(feature) {
  let md = `# ${feature.title} Specification\n\n`;
  md += `## Purpose\n${feature.description || feature.title}\n\n`;
  md += `## Requirements\n\n`;

  for (const sc of feature.scenarios) {
    // Derive a SHALL statement from the first Then step
    const thenStep = sc.steps.find((s) => s.keyword === "Then");
    const shall = thenStep
      ? `The system SHALL ${thenStep.text.replace(/^the\s+/i, "").replace(/^a\s+/i, "")}.`
      : `The system SHALL support ${sc.title}.`;

    md += `### Requirement: ${sc.title}\n${shall}\n\n`;
    md += `#### Scenario: ${sc.title}\n`;
    for (const step of sc.steps) {
      md += `- ${step.keyword.toUpperCase()} ${step.text}\n`;
    }
    md += `\n`;
  }
  return md;
}

/** Convert parsed Gherkin into OpenSpec tasks.md format. */
function toTasksMd(feature) {
  let md = `# Tasks\n\n`;
  feature.scenarios.forEach((sc, si) => {
    const num = si + 1;
    md += `## ${num}. ${sc.title}\n`;
    let sub = 1;
    for (const step of sc.steps) {
      const verb =
        step.keyword === "Given"
          ? "Set up"
          : step.keyword === "When"
            ? "Implement"
            : step.keyword === "Then"
              ? "Verify"
              : "Handle";
      md += `- [ ] ${num}.${sub} ${verb}: ${step.text}\n`;
      sub++;
    }
    md += `\n`;
  });
  return md;
}

/** Write a minimal design.md stub. */
function toDesignMd(feature) {
  return (
    `# Design: ${feature.title}\n\n` +
    `## Technical Approach\n_To be completed._\n\n` +
    `## File Changes\n_To be completed after implementation planning._\n`
  );
}

// ─────────────────────────────────────────────
// Proposal generation via Ollama
// ─────────────────────────────────────────────

async function generateProposal(feature, gherkinText) {
  const prompt =
    `You are a technical writer. Given the following Gherkin Feature file, ` +
    `produce a concise OpenSpec proposal with these exact three markdown sections:\n` +
    `## Intent\n(1-2 sentences: what problem does this solve?)\n` +
    `## Scope\nIn scope:\n- ...\n\nOut of scope:\n- ...\n` +
    `## Approach\n(1 paragraph: high-level technical approach)\n\n` +
    `Do not output Gherkin. Do not add extra sections. Be concise.\n\n` +
    `=== Gherkin Feature ===\n${gherkinText}\n`;

  try {
    const resp = await fetch(`${OLLAMA_URL}/api/generate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: OLLAMA_MODEL,
        prompt,
        stream: false,
      }),
      signal: AbortSignal.timeout(180_000),
    });
    if (!resp.ok) throw new Error(`Ollama returned ${resp.status}`);
    const body = await resp.json();
    return `# Proposal: ${feature.title}\n\n${body.response.trim()}\n`;
  } catch (err) {
    console.error("[proposal] LLM call failed:", err.message);
    // Fallback: deterministic proposal from Gherkin structure
    return (
      `# Proposal: ${feature.title}\n\n` +
      `## Intent\n${feature.description || feature.title}\n\n` +
      `## Scope\nIn scope:\n` +
      feature.scenarios.map((s) => `- ${s.title}`).join("\n") +
      `\n\nOut of scope:\n- _To be defined_\n\n` +
      `## Approach\n_To be completed._\n`
    );
  }
}

// ─────────────────────────────────────────────
// Validation via OpenSpec CLI
// ─────────────────────────────────────────────

function validateChange(changeName) {
  try {
    const out = execFileSync(
      "openspec",
      ["validate", changeName, "--json"],
      { cwd: WORKSPACE, stdio: "pipe", timeout: 15_000 }
    );
    return JSON.parse(out.toString());
  } catch {
    return null; // CLI not available or validation failed
  }
}

// ─────────────────────────────────────────────
// /generate handler
// ─────────────────────────────────────────────

async function handleGenerate(body) {
  const {
    change_name,
    gherkin,
    generate_proposal = true,
    ollama_url,
    ollama_model,
  } = body;

  if (!change_name || !gherkin) {
    return { status: 400, body: { error: "change_name and gherkin are required" } };
  }

  // Allow per-request Ollama overrides
  const ollamaUrlOverride = ollama_url || OLLAMA_URL;
  const ollamaModelOverride = ollama_model || OLLAMA_MODEL;

  // Parse the Gherkin
  const feature = parseGherkin(gherkin);

  // Build artifacts
  const artifacts = {};

  // spec.md
  const domain = change_name.replace(/[^a-z0-9-]/gi, "-").toLowerCase();
  artifacts[`specs/${domain}/spec.md`] = toSpecMd(feature);

  // tasks.md
  artifacts["tasks.md"] = toTasksMd(feature);

  // design.md
  artifacts["design.md"] = toDesignMd(feature);

  // proposal.md — via LLM or fallback
  if (generate_proposal) {
    // Temporarily override globals for this request
    const savedUrl = OLLAMA_URL;
    const savedModel = OLLAMA_MODEL;
    // Use a closure approach to avoid mutating module globals
    const proposalText = await generateProposalWithConfig(
      feature,
      gherkin,
      ollamaUrlOverride,
      ollamaModelOverride
    );
    artifacts["proposal.md"] = proposalText;
  } else {
    artifacts["proposal.md"] =
      `# Proposal: ${feature.title}\n\n` +
      `## Intent\n${feature.description || feature.title}\n\n` +
      `## Scope\nIn scope:\n` +
      feature.scenarios.map((s) => `- ${s.title}`).join("\n") +
      `\n\nOut of scope:\n- _To be defined_\n\n` +
      `## Approach\n_To be completed._\n`;
  }

  // Write to disk inside the OpenSpec workspace
  const changeDir = join(WORKSPACE, "openspec", "changes", change_name);
  mkdirSync(changeDir, { recursive: true });

  for (const [relPath, content] of Object.entries(artifacts)) {
    const fullPath = join(changeDir, relPath);
    mkdirSync(join(fullPath, ".."), { recursive: true });
    writeFileSync(fullPath, content, "utf8");
  }

  // Validate if CLI is available
  const validation = validateChange(change_name);

  return {
    status: 200,
    body: {
      success: true,
      change_name,
      feature_title: feature.title,
      scenario_count: feature.scenarios.length,
      artifacts,
      validation,
      output_dir: changeDir,
    },
  };
}

/** Generate proposal with specific Ollama config (per-request overrides). */
async function generateProposalWithConfig(feature, gherkinText, url, model) {
  const prompt =
    `You are a technical writer. Given the following Gherkin Feature file, ` +
    `produce a concise OpenSpec proposal with these exact three markdown sections:\n` +
    `## Intent\n(1-2 sentences: what problem does this solve?)\n` +
    `## Scope\nIn scope:\n- ...\n\nOut of scope:\n- ...\n` +
    `## Approach\n(1 paragraph: high-level technical approach)\n\n` +
    `Do not output Gherkin. Do not add extra sections. Be concise.\n\n` +
    `=== Gherkin Feature ===\n${gherkinText}\n`;

  try {
    const resp = await fetch(`${url}/api/generate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model, prompt, stream: false }),
      signal: AbortSignal.timeout(180_000),
    });
    if (!resp.ok) throw new Error(`Ollama returned ${resp.status}`);
    const body = await resp.json();
    return `# Proposal: ${feature.title}\n\n${body.response.trim()}\n`;
  } catch (err) {
    console.error("[proposal] LLM call failed:", err.message);
    return (
      `# Proposal: ${feature.title}\n\n` +
      `## Intent\n${feature.description || feature.title}\n\n` +
      `## Scope\nIn scope:\n` +
      feature.scenarios.map((s) => `- ${s.title}`).join("\n") +
      `\n\nOut of scope:\n- _To be defined_\n\n` +
      `## Approach\n_To be completed._\n`
    );
  }
}

// ─────────────────────────────────────────────
// HTTP server
// ─────────────────────────────────────────────

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    const MAX_BODY = 10 * 1024 * 1024; // 10 MB
    req.on("data", (chunk) => {
      size += chunk.length;
      if (size > MAX_BODY) {
        req.destroy();
        reject(new Error("Body too large"));
      }
      chunks.push(chunk);
    });
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

function sendJson(res, status, body) {
  const json = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Content-Length": Buffer.byteLength(json),
  });
  res.end(json);
}

const server = createServer(async (req, res) => {
  try {
    if (req.method === "GET" && req.url === "/health") {
      sendJson(res, 200, { status: "ok" });
      return;
    }

    if (req.method === "POST" && req.url === "/generate") {
      const raw = await readBody(req);
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        sendJson(res, 400, { error: "Invalid JSON" });
        return;
      }
      const result = await handleGenerate(body);
      sendJson(res, result.status, result.body);
      return;
    }

    sendJson(res, 404, { error: "Not found" });
  } catch (err) {
    console.error("[server]", err);
    sendJson(res, 500, { error: err.message || "Internal server error" });
  }
});

// Boot
ensureWorkspace();
server.listen(PORT, "0.0.0.0", () => {
  console.log(`[openspec-service] Listening on :${PORT}`);
  console.log(`[openspec-service] Ollama URL: ${OLLAMA_URL}`);
  console.log(`[openspec-service] Workspace:  ${WORKSPACE}`);
});
