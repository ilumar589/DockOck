//! Dependency graph data-structures, parser, and rendering helpers.
//!
//! A `DependencyGraph` captures business entities, their state lifecycles,
//! business rules, and inter-entity dependencies extracted from documents.
//! It is the output artefact when the user selects `OutputMode::DependencyGraph`
//! instead of `OutputMode::Gherkin`.

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────
// Node types
// ─────────────────────────────────────────────

/// Classification of a graph node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntityType {
    Actor,
    System,
    DataObject,
    Process,
    Service,
    ExternalSystem,
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Actor => write!(f, "Actor"),
            Self::System => write!(f, "System"),
            Self::DataObject => write!(f, "DataObject"),
            Self::Process => write!(f, "Process"),
            Self::Service => write!(f, "Service"),
            Self::ExternalSystem => write!(f, "ExternalSystem"),
        }
    }
}

/// Whether a business rule applies to setup/configuration or runtime objects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuleCategory {
    Setup,
    Runtime,
}

/// A single state in an entity's lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// A transition between two states.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub from_state: String,
    pub to_state: String,
    pub trigger: String,
    #[serde(default)]
    pub guards: Vec<String>,
}

/// A business rule attached to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessRule {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub lifecycle_phases: Vec<String>,
    #[serde(default = "default_rule_category")]
    pub category: RuleCategory,
}

fn default_rule_category() -> RuleCategory {
    RuleCategory::Runtime
}

/// A node in the dependency graph — one business entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub entity_type: EntityType,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub states: Vec<State>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    #[serde(default)]
    pub rules: Vec<BusinessRule>,
    #[serde(default)]
    pub source_documents: Vec<String>,
}

// ─────────────────────────────────────────────
// Edge types
// ─────────────────────────────────────────────

/// The kind of relationship an edge represents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EdgeRelationship {
    DependsOn,
    Triggers,
    Contains,
    Produces,
    Consumes,
    Validates,
    Extends,
    References,
}

impl std::fmt::Display for EdgeRelationship {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DependsOn => write!(f, "depends_on"),
            Self::Triggers => write!(f, "triggers"),
            Self::Contains => write!(f, "contains"),
            Self::Produces => write!(f, "produces"),
            Self::Consumes => write!(f, "consumes"),
            Self::Validates => write!(f, "validates"),
            Self::Extends => write!(f, "extends"),
            Self::References => write!(f, "references"),
        }
    }
}

/// A directed edge between two graph nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from_node: String,
    pub to_node: String,
    pub relationship: EdgeRelationship,
    #[serde(default)]
    pub label: String,
}

// ─────────────────────────────────────────────
// DependencyGraph
// ─────────────────────────────────────────────

/// The complete dependency graph for a file or group of files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyGraph {
    pub title: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub source_files: Vec<String>,
}

impl DependencyGraph {
    /// Parse an LLM-generated raw text block into a `DependencyGraph`.
    ///
    /// The LLM is instructed to output JSON, but its response may include
    /// surrounding prose or markdown fences. This parser strips those and
    /// does a best-effort JSON parse with a fallback for malformed output.
    pub fn parse_from_llm_output(raw: &str, source_files: &[&str]) -> Self {
        let cleaned = strip_json_fences(raw);

        match serde_json::from_str::<DependencyGraph>(&cleaned) {
            Ok(mut graph) => {
                // Inject source files into nodes that lack them
                if graph.source_files.is_empty() {
                    graph.source_files = source_files.iter().map(|s| s.to_string()).collect();
                }
                for node in &mut graph.nodes {
                    if node.source_documents.is_empty() {
                        node.source_documents =
                            source_files.iter().map(|s| s.to_string()).collect();
                    }
                }
                graph
            }
            Err(e) => {
                tracing::warn!("Failed to parse dependency graph JSON: {e} — building minimal graph");
                DependencyGraph {
                    title: "Generated Dependency Graph".to_string(),
                    nodes: vec![GraphNode {
                        id: "parse_error".to_string(),
                        name: "Parse Error".to_string(),
                        entity_type: EntityType::System,
                        description: format!(
                            "LLM output could not be parsed as JSON: {e}\n\nRaw output:\n{raw}"
                        ),
                        states: Vec::new(),
                        transitions: Vec::new(),
                        rules: Vec::new(),
                        source_documents: source_files.iter().map(|s| s.to_string()).collect(),
                    }],
                    edges: Vec::new(),
                    source_files: source_files.iter().map(|s| s.to_string()).collect(),
                }
            }
        }
    }

    /// Render the graph as a Mermaid diagram string.
    pub fn to_mermaid(&self) -> String {
        let mut out = String::from("graph TD\n");

        // Nodes with shape based on entity type
        for node in &self.nodes {
            let id = mermaid_id(&node.id);
            let label = mermaid_escape(&node.name);
            let shape = match node.entity_type {
                EntityType::Actor => format!("  {}([{}])", id, label),
                EntityType::Process => {
                    format!("  {}{{{{{}}}}}", id, label)
                }
                EntityType::System | EntityType::Service => {
                    format!("  {}[[{}]]", id, label)
                }
                EntityType::ExternalSystem => {
                    format!("  {}[({})]", id, label)
                }
                EntityType::DataObject => {
                    format!("  {}[{}]", id, label)
                }
            };
            out.push_str(&shape);
            out.push('\n');
        }

        out.push('\n');

        // Edges
        for edge in &self.edges {
            let from = mermaid_id(&edge.from_node);
            let to = mermaid_id(&edge.to_node);
            let arrow = match edge.relationship {
                EdgeRelationship::DependsOn => "-->",
                EdgeRelationship::Triggers => "-.->",
                EdgeRelationship::Contains => "---",
                _ => "-->",
            };
            if edge.label.is_empty() {
                out.push_str(&format!(
                    "  {} {} {}\n",
                    from, arrow, to
                ));
            } else {
                out.push_str(&format!(
                    "  {} {}|{}| {}\n",
                    from,
                    arrow,
                    mermaid_escape(&edge.label),
                    to
                ));
            }
        }

        out
    }

    /// Render as a DOT (Graphviz) string.
    pub fn to_dot(&self) -> String {
        let mut out = String::from("digraph DependencyGraph {\n  rankdir=TB;\n  node [fontname=\"Helvetica\"];\n\n");

        for node in &self.nodes {
            let shape = match node.entity_type {
                EntityType::Actor => "ellipse",
                EntityType::Process => "diamond",
                EntityType::System | EntityType::Service => "box",
                EntityType::ExternalSystem => "box3d",
                EntityType::DataObject => "rectangle",
            };
            out.push_str(&format!(
                "  {} [label=\"{}\" shape={}];\n",
                node.id,
                dot_escape(&node.name),
                shape,
            ));
        }

        out.push('\n');

        for edge in &self.edges {
            let style = match edge.relationship {
                EdgeRelationship::Triggers => " style=dashed",
                EdgeRelationship::Contains => " style=dotted",
                _ => "",
            };
            let label = if edge.label.is_empty() {
                edge.relationship.to_string()
            } else {
                format!("{}: {}", edge.relationship, edge.label)
            };
            out.push_str(&format!(
                "  {} -> {} [label=\"{}\"{}];\n",
                edge.from_node,
                edge.to_node,
                dot_escape(&label),
                style,
            ));
        }

        out.push_str("}\n");
        out
    }

    /// Render as formatted JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Generate a self-contained HTML page that renders the Mermaid diagram
    /// in a browser. Uses the Mermaid CDN — no local tools required.
    pub fn to_visual_html(&self) -> String {
        let mermaid_src = self.to_mermaid()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let summary = self.to_summary_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\n', "<br>\n");
        format!(
r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; background: #1e1e2e; color: #cdd6f4; overflow: hidden; height: 100vh; display: flex; flex-direction: column; }}
  .toolbar {{ background: #181825; border-bottom: 1px solid #45475a; padding: 8px 16px; display: flex; align-items: center; gap: 10px; flex-shrink: 0; }}
  .toolbar h1 {{ color: #89b4fa; font-size: 16px; margin-right: auto; }}
  .toolbar button {{ background: #45475a; color: #cdd6f4; border: none; padding: 6px 14px; border-radius: 6px; cursor: pointer; font-size: 13px; }}
  .toolbar button:hover {{ background: #585b70; }}
  .toolbar .zoom-info {{ font-size: 12px; color: #a6adc8; min-width: 50px; text-align: center; }}
  .graph-viewport {{ flex: 1; overflow: hidden; position: relative; cursor: grab; background: #1e1e2e; }}
  .graph-viewport:active {{ cursor: grabbing; }}
  .graph-inner {{ transform-origin: 0 0; position: absolute; top: 0; left: 0; }}
  .graph-inner svg {{ display: block; }}
  .summary-overlay {{ display: none; position: fixed; top: 50px; right: 16px; width: 500px; max-height: calc(100vh - 70px); overflow: auto; background: #181825; border: 1px solid #45475a; border-radius: 8px; padding: 16px; font-size: 13px; line-height: 1.6; white-space: pre-wrap; z-index: 100; box-shadow: 0 8px 32px rgba(0,0,0,.5); }}
  .summary-overlay.visible {{ display: block; }}
</style>
</head>
<body>
<div class="toolbar">
  <h1>📊 {title}</h1>
  <button onclick="zoomTo(1)">Fit</button>
  <button onclick="zoomBy(1.3)">➕</button>
  <button onclick="zoomBy(0.7)">➖</button>
  <span class="zoom-info" id="zoomInfo">100%</span>
  <button onclick="downloadSvg()">⬇ SVG</button>
  <button onclick="toggleSummary()">📄 Summary</button>
</div>
<div class="graph-viewport" id="viewport">
  <div class="graph-inner" id="graphInner">
    <pre class="mermaid">
{mermaid}
    </pre>
  </div>
</div>
<div class="summary-overlay" id="summary">{summary}</div>
<script src="https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js"></script>
<script>
  mermaid.initialize({{ startOnLoad: true, theme: 'dark', securityLevel: 'loose', maxTextSize: 500000,
    flowchart: {{ nodeSpacing: 30, rankSpacing: 60, padding: 15, useMaxWidth: false }} }});

  const vp = document.getElementById('viewport');
  const inner = document.getElementById('graphInner');
  let scale = 1, tx = 0, ty = 0, dragging = false, sx = 0, sy = 0;

  function applyTransform() {{
    inner.style.transform = `translate(${{tx}}px,${{ty}}px) scale(${{scale}})`;
    document.getElementById('zoomInfo').textContent = Math.round(scale * 100) + '%';
  }}

  /* Fit the SVG into the viewport */
  function zoomTo(mode) {{
    const svg = inner.querySelector('svg');
    if (!svg) return;
    const vw = vp.clientWidth, vh = vp.clientHeight;
    const sw = svg.scrollWidth || svg.clientWidth, sh = svg.scrollHeight || svg.clientHeight;
    if (mode === 1) {{
      scale = Math.min(vw / sw, vh / sh, 1) * 0.95;
      tx = (vw - sw * scale) / 2;
      ty = (vh - sh * scale) / 2;
    }}
    applyTransform();
  }}
  function zoomBy(f) {{
    const vw = vp.clientWidth / 2, vh = vp.clientHeight / 2;
    tx = vw - f * (vw - tx);
    ty = vh - f * (vh - ty);
    scale *= f;
    applyTransform();
  }}

  /* Mouse wheel zoom */
  vp.addEventListener('wheel', (e) => {{
    e.preventDefault();
    const f = e.deltaY < 0 ? 1.1 : 0.9;
    const rect = vp.getBoundingClientRect();
    const mx = e.clientX - rect.left, my = e.clientY - rect.top;
    tx = mx - f * (mx - tx);
    ty = my - f * (my - ty);
    scale *= f;
    applyTransform();
  }}, {{ passive: false }});

  /* Pan with mouse drag */
  vp.addEventListener('mousedown', (e) => {{ dragging = true; sx = e.clientX - tx; sy = e.clientY - ty; }});
  window.addEventListener('mousemove', (e) => {{ if (!dragging) return; tx = e.clientX - sx; ty = e.clientY - sy; applyTransform(); }});
  window.addEventListener('mouseup', () => {{ dragging = false; }});

  function toggleSummary() {{ document.getElementById('summary').classList.toggle('visible'); }}
  function downloadSvg() {{
    const svg = inner.querySelector('svg');
    if (!svg) {{ alert('Graph not rendered yet'); return; }}
    const blob = new Blob([svg.outerHTML], {{ type: 'image/svg+xml' }});
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = '{title}.svg';
    a.click();
  }}

  /* Auto-fit once Mermaid finishes rendering */
  const observer = new MutationObserver(() => {{
    if (inner.querySelector('svg')) {{ observer.disconnect(); setTimeout(() => zoomTo(1), 100); }}
  }});
  observer.observe(inner, {{ childList: true, subtree: true }});
</script>
</body>
</html>"#,
            title = self.title.replace('"', "&quot;"),
            mermaid = mermaid_src,
            summary = summary,
        )
    }

    /// Try to render the DOT source to SVG using an external Graphviz `dot` binary.
    /// Returns `Ok(svg_string)` on success, or `Err` if `dot` is not installed.
    pub fn render_dot_to_svg(&self) -> Result<String, String> {
        use std::process::Command;
        let dot_src = self.to_dot();
        let child = Command::new("dot")
            .args(["-Tsvg"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Graphviz 'dot' not found ({}). Install from https://graphviz.org", e))?;

        use std::io::Write;
        let mut child = child;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(dot_src.as_bytes()).map_err(|e| format!("Failed to write to dot stdin: {e}"))?;
        }
        let output = child.wait_with_output().map_err(|e| format!("dot process failed: {e}"))?;
        if output.status.success() {
            String::from_utf8(output.stdout).map_err(|e| format!("dot output not valid UTF-8: {e}"))
        } else {
            Err(format!("dot returned error: {}", String::from_utf8_lossy(&output.stderr)))
        }
    }

    /// Render the full graph as a human-readable summary string (for UI display).
    pub fn to_summary_string(&self) -> String {
        let mut out = format!("# {}\n\n", self.title);

        out.push_str(&format!(
            "**{} entities, {} dependencies**\n\n",
            self.nodes.len(),
            self.edges.len()
        ));

        for node in &self.nodes {
            out.push_str(&format!("## {} ({})\n", node.name, node.entity_type));
            if !node.description.is_empty() {
                out.push_str(&format!("{}\n", node.description));
            }
            if !node.states.is_empty() {
                out.push_str("\n**States:** ");
                let state_names: Vec<&str> = node.states.iter().map(|s| s.name.as_str()).collect();
                out.push_str(&state_names.join(" → "));
                out.push('\n');
            }
            if !node.transitions.is_empty() {
                out.push_str("\n**Transitions:**\n");
                for tr in &node.transitions {
                    out.push_str(&format!("  {} → {} ({})\n", tr.from_state, tr.to_state, tr.trigger));
                    if !tr.guards.is_empty() {
                        out.push_str(&format!("    Guards: {}\n", tr.guards.join(", ")));
                    }
                }
            }
            if !node.rules.is_empty() {
                out.push_str("\n**Business Rules:**\n");
                for rule in &node.rules {
                    out.push_str(&format!(
                        "  [{}] {} — {}\n",
                        rule.id, rule.description,
                        rule.lifecycle_phases.join(", ")
                    ));
                }
            }
            out.push('\n');
        }

        if !self.edges.is_empty() {
            out.push_str("## Dependencies\n\n");
            for edge in &self.edges {
                out.push_str(&format!(
                    "  {} --[{}]--> {}",
                    edge.from_node, edge.relationship, edge.to_node
                ));
                if !edge.label.is_empty() {
                    out.push_str(&format!(" ({})", edge.label));
                }
                out.push('\n');
            }
        }

        out
    }
}

impl GraphNode {
    /// Render this entity node as a prose paragraph suitable for embedding.
    pub fn to_embeddable_text(&self) -> String {
        let mut text = format!("Entity: {} ({})", self.name, self.entity_type);
        if !self.description.is_empty() {
            text.push_str(&format!("\nDescription: {}", self.description));
        }
        if !self.states.is_empty() {
            let names: Vec<&str> = self.states.iter().map(|s| s.name.as_str()).collect();
            text.push_str(&format!("\nStates: {}", names.join(" → ")));
        }
        if !self.transitions.is_empty() {
            text.push_str("\nTransitions:");
            for tr in &self.transitions {
                text.push_str(&format!(" {} → {} ({})", tr.from_state, tr.to_state, tr.trigger));
                if !tr.guards.is_empty() {
                    text.push_str(&format!(" [guards: {}]", tr.guards.join(", ")));
                }
            }
        }
        if !self.rules.is_empty() {
            text.push_str("\nRules:");
            for rule in &self.rules {
                text.push_str(&format!(" {}: {}", rule.id, rule.description));
            }
        }
        text
    }
}

impl BusinessRule {
    /// Render this rule as a prose sentence suitable for embedding.
    pub fn to_embeddable_text(&self, entity_name: &str) -> String {
        let mut text = format!(
            "{}: {}. Category: {:?}. Entity: {}.",
            self.id, self.description, self.category, entity_name,
        );
        if !self.lifecycle_phases.is_empty() {
            text.push_str(&format!(" Lifecycle phases: {}.", self.lifecycle_phases.join(", ")));
        }
        text
    }
}

// ─────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────

/// Strip markdown code fences and find the JSON object in raw LLM output.
fn strip_json_fences(raw: &str) -> String {
    let mut text = raw.trim().to_string();

    // Remove ```json ... ``` fences
    if text.starts_with("```") {
        if let Some(end) = text.rfind("```") {
            if end > 3 {
                // Skip first line (```json) and trailing ```
                let start = text.find('\n').map(|i| i + 1).unwrap_or(3);
                text = text[start..end].to_string();
            }
        }
    }

    // Find first '{' and last '}'
    let start = text.find('{');
    let end = text.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if s < e => text[s..=e].to_string(),
        _ => text,
    }
}

/// Escape characters that break Mermaid syntax.
/// Uses Mermaid's quoted label syntax to safely wrap any text.
fn mermaid_escape(s: &str) -> String {
    // Inside quoted strings ("..."), we need to escape:
    //  - " (would close the label)
    //  - | (breaks edge-label delimiters)
    //  - # (starts Mermaid HTML-entity refs like #amp;)
    //  - newlines (break line-based parsing)
    let cleaned = s
        .replace('"', "'")
        .replace('|', "/")
        .replace('#', "Nr.")
        .replace('\n', " ")
        .replace('\r', "");
    format!("\"{}\"" , cleaned)
}

/// Sanitize a raw identifier for use as a Mermaid node ID.
/// Prefixed with `n` so IDs never start with reserved words like `end`.
fn mermaid_id(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    format!("n{}", sanitized)
}

/// Escape characters for DOT label strings.
fn dot_escape(s: &str) -> String {
    s.replace('"', "\\\"").replace('\n', "\\n")
}

// ─────────────────────────────────────────────
// Merging
// ─────────────────────────────────────────────

/// Merge multiple dependency graphs into a single combined graph.
/// Nodes with duplicate IDs are merged (later wins for fields, source_documents accumulate).
/// Edges are deduplicated by (from, to, relationship).
pub fn merge_graphs(graphs: &[&DependencyGraph]) -> DependencyGraph {
    use std::collections::{HashMap, HashSet};

    let mut node_map: HashMap<String, GraphNode> = HashMap::new();
    let mut edge_set: HashSet<String> = HashSet::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut source_files: Vec<String> = Vec::new();
    let mut titles: Vec<&str> = Vec::new();

    for graph in graphs {
        titles.push(&graph.title);
        for sf in &graph.source_files {
            if !source_files.contains(sf) {
                source_files.push(sf.clone());
            }
        }
        for node in &graph.nodes {
            if let Some(existing) = node_map.get_mut(&node.id) {
                // Merge source documents
                for doc in &node.source_documents {
                    if !existing.source_documents.contains(doc) {
                        existing.source_documents.push(doc.clone());
                    }
                }
                // Merge states (by name)
                let existing_state_names: HashSet<String> = existing.states.iter().map(|s| s.name.clone()).collect();
                for state in &node.states {
                    if !existing_state_names.contains(&state.name) {
                        existing.states.push(state.clone());
                    }
                }
                // Merge transitions
                let existing_trans: HashSet<String> = existing.transitions.iter()
                    .map(|t| format!("{}->{}:{}", t.from_state, t.to_state, t.trigger))
                    .collect();
                for tr in &node.transitions {
                    let key = format!("{}->{}:{}", tr.from_state, tr.to_state, tr.trigger);
                    if !existing_trans.contains(&key) {
                        existing.transitions.push(tr.clone());
                    }
                }
                // Merge rules (by id)
                let existing_rule_ids: HashSet<String> = existing.rules.iter().map(|r| r.id.clone()).collect();
                for rule in &node.rules {
                    if !existing_rule_ids.contains(&rule.id) {
                        existing.rules.push(rule.clone());
                    }
                }
                // Update description if empty
                if existing.description.is_empty() && !node.description.is_empty() {
                    existing.description = node.description.clone();
                }
            } else {
                node_map.insert(node.id.clone(), node.clone());
            }
        }
        for edge in &graph.edges {
            let key = format!("{}->{}:{}", edge.from_node, edge.to_node, edge.relationship);
            if edge_set.insert(key) {
                edges.push(edge.clone());
            }
        }
    }

    // Sort nodes by id for stable output
    let mut nodes: Vec<GraphNode> = node_map.into_values().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    DependencyGraph {
        title: format!("Combined Dependency Graph ({} sources)", graphs.len()),
        nodes,
        edges,
        source_files,
    }
}

// ─────────────────────────────────────────────
// Diffing
// ─────────────────────────────────────────────

/// A single change between two dependency graphs.
#[derive(Debug, Clone)]
pub enum GraphDiffEntry {
    NodeAdded(String),
    NodeRemoved(String),
    NodeModified { id: String, detail: String },
    EdgeAdded { from: String, to: String, rel: String },
    EdgeRemoved { from: String, to: String, rel: String },
}

/// Compute a diff between two dependency graphs.
pub fn diff_depgraph(old: &DependencyGraph, new: &DependencyGraph) -> Vec<GraphDiffEntry> {
    use std::collections::{HashMap, HashSet};
    let mut result = Vec::new();

    let old_nodes: HashMap<&str, &GraphNode> = old.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let new_nodes: HashMap<&str, &GraphNode> = new.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let old_ids: HashSet<&str> = old_nodes.keys().copied().collect();
    let new_ids: HashSet<&str> = new_nodes.keys().copied().collect();

    // Added nodes
    for id in new_ids.difference(&old_ids) {
        result.push(GraphDiffEntry::NodeAdded(id.to_string()));
    }

    // Removed nodes
    for id in old_ids.difference(&new_ids) {
        result.push(GraphDiffEntry::NodeRemoved(id.to_string()));
    }

    // Modified nodes — compare serialized form for simplicity
    for id in old_ids.intersection(&new_ids) {
        let old_json = serde_json::to_string(old_nodes[id]).unwrap_or_default();
        let new_json = serde_json::to_string(new_nodes[id]).unwrap_or_default();
        if old_json != new_json {
            let old_n = old_nodes[id];
            let new_n = new_nodes[id];
            let mut details = Vec::new();
            if old_n.states.len() != new_n.states.len() {
                details.push(format!(
                    "states: {} → {}",
                    old_n.states.len(),
                    new_n.states.len()
                ));
            }
            if old_n.transitions.len() != new_n.transitions.len() {
                details.push(format!(
                    "transitions: {} → {}",
                    old_n.transitions.len(),
                    new_n.transitions.len()
                ));
            }
            if old_n.rules.len() != new_n.rules.len() {
                details.push(format!(
                    "rules: {} → {}",
                    old_n.rules.len(),
                    new_n.rules.len()
                ));
            }
            if old_n.description != new_n.description {
                details.push("description changed".to_string());
            }
            let detail = if details.is_empty() {
                "content changed".to_string()
            } else {
                details.join(", ")
            };
            result.push(GraphDiffEntry::NodeModified {
                id: id.to_string(),
                detail,
            });
        }
    }

    // Edge diff — key edges by (from, to, relationship)
    let edge_key = |e: &GraphEdge| {
        format!("{}->{}:{}", e.from_node, e.to_node, e.relationship)
    };
    let old_edges: HashSet<String> = old.edges.iter().map(edge_key).collect();
    let new_edges: HashSet<String> = new.edges.iter().map(edge_key).collect();

    for e in &new.edges {
        let key = edge_key(e);
        if !old_edges.contains(&key) {
            result.push(GraphDiffEntry::EdgeAdded {
                from: e.from_node.clone(),
                to: e.to_node.clone(),
                rel: e.relationship.to_string(),
            });
        }
    }

    for e in &old.edges {
        let key = edge_key(e);
        if !new_edges.contains(&key) {
            result.push(GraphDiffEntry::EdgeRemoved {
                from: e.from_node.clone(),
                to: e.to_node.clone(),
                rel: e.relationship.to_string(),
            });
        }
    }

    result
}

// ─────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_json() {
        let raw = r#"```json
{
  "title": "Order System",
  "nodes": [
    {
      "id": "order",
      "name": "Order",
      "entity_type": "DataObject",
      "description": "A customer order",
      "states": [
        {"name": "Draft", "description": "Initial state"},
        {"name": "Confirmed", "description": "Order confirmed"}
      ],
      "transitions": [
        {"from_state": "Draft", "to_state": "Confirmed", "trigger": "Customer confirms", "guards": ["all items in stock"]}
      ],
      "rules": [
        {"id": "BR-001", "description": "Order must have at least one line item", "lifecycle_phases": ["Creation"], "category": "Runtime"}
      ],
      "source_documents": ["D028.docx"]
    },
    {
      "id": "customer",
      "name": "Customer",
      "entity_type": "Actor",
      "description": "End user"
    }
  ],
  "edges": [
    {"from_node": "order", "to_node": "customer", "relationship": "DependsOn", "label": "placed by"}
  ]
}
```"#;

        let graph = DependencyGraph::parse_from_llm_output(raw, &["D028.docx"]);
        assert_eq!(graph.title, "Order System");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.nodes[0].states.len(), 2);
        assert_eq!(graph.nodes[0].transitions.len(), 1);
        assert_eq!(graph.nodes[0].rules.len(), 1);
        assert_eq!(graph.edges[0].relationship, EdgeRelationship::DependsOn);
    }

    #[test]
    fn test_parse_malformed_json_fallback() {
        let raw = "This is not JSON at all, the model went off-script.";
        let graph = DependencyGraph::parse_from_llm_output(raw, &["test.docx"]);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, "parse_error");
        assert!(graph.nodes[0].description.contains("Raw output:"));
    }

    #[test]
    fn test_mermaid_rendering() {
        let graph = DependencyGraph {
            title: "Test".to_string(),
            nodes: vec![
                GraphNode {
                    id: "a".to_string(),
                    name: "Actor A".to_string(),
                    entity_type: EntityType::Actor,
                    description: String::new(),
                    states: Vec::new(),
                    transitions: Vec::new(),
                    rules: Vec::new(),
                    source_documents: Vec::new(),
                },
                GraphNode {
                    id: "b".to_string(),
                    name: "System B".to_string(),
                    entity_type: EntityType::System,
                    description: String::new(),
                    states: Vec::new(),
                    transitions: Vec::new(),
                    rules: Vec::new(),
                    source_documents: Vec::new(),
                },
            ],
            edges: vec![GraphEdge {
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                relationship: EdgeRelationship::DependsOn,
                label: String::new(),
            }],
            source_files: Vec::new(),
        };

        let mermaid = graph.to_mermaid();
        assert!(mermaid.contains("graph TD"));
        assert!(mermaid.contains("a([Actor A])"));
        assert!(mermaid.contains("b[[System B]]"));
        assert!(mermaid.contains("a --> b"));
    }

    #[test]
    fn test_diff_detects_changes() {
        let old = DependencyGraph {
            title: "V1".to_string(),
            nodes: vec![GraphNode {
                id: "a".to_string(),
                name: "A".to_string(),
                entity_type: EntityType::Actor,
                description: "old desc".to_string(),
                states: Vec::new(),
                transitions: Vec::new(),
                rules: Vec::new(),
                source_documents: Vec::new(),
            }],
            edges: Vec::new(),
            source_files: Vec::new(),
        };

        let new = DependencyGraph {
            title: "V2".to_string(),
            nodes: vec![
                GraphNode {
                    id: "a".to_string(),
                    name: "A".to_string(),
                    entity_type: EntityType::Actor,
                    description: "new desc".to_string(),
                    states: Vec::new(),
                    transitions: Vec::new(),
                    rules: Vec::new(),
                    source_documents: Vec::new(),
                },
                GraphNode {
                    id: "b".to_string(),
                    name: "B".to_string(),
                    entity_type: EntityType::System,
                    description: String::new(),
                    states: Vec::new(),
                    transitions: Vec::new(),
                    rules: Vec::new(),
                    source_documents: Vec::new(),
                },
            ],
            edges: vec![GraphEdge {
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                relationship: EdgeRelationship::Triggers,
                label: String::new(),
            }],
            source_files: Vec::new(),
        };

        let diff = diff_depgraph(&old, &new);
        assert!(diff.iter().any(|d| matches!(d, GraphDiffEntry::NodeAdded(id) if id == "b")));
        assert!(diff.iter().any(|d| matches!(d, GraphDiffEntry::NodeModified { id, .. } if id == "a")));
        assert!(diff.iter().any(|d| matches!(d, GraphDiffEntry::EdgeAdded { .. })));
    }

    #[test]
    fn test_json_roundtrip() {
        let graph = DependencyGraph {
            title: "Test".to_string(),
            nodes: vec![GraphNode {
                id: "x".to_string(),
                name: "X".to_string(),
                entity_type: EntityType::DataObject,
                description: "desc".to_string(),
                states: vec![State {
                    name: "Active".to_string(),
                    description: "Running".to_string(),
                }],
                transitions: Vec::new(),
                rules: vec![BusinessRule {
                    id: "BR-1".to_string(),
                    description: "Must be valid".to_string(),
                    lifecycle_phases: vec!["Creation".to_string()],
                    category: RuleCategory::Runtime,
                }],
                source_documents: vec!["test.docx".to_string()],
            }],
            edges: Vec::new(),
            source_files: vec!["test.docx".to_string()],
        };

        let json = graph.to_json();
        let parsed: DependencyGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, "Test");
        assert_eq!(parsed.nodes[0].states.len(), 1);
        assert_eq!(parsed.nodes[0].rules[0].id, "BR-1");
    }
}
