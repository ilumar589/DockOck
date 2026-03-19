# Gherkin Quality Improvement Plan

**Source:** Itineris Feedback on Gherkin Scenarios (March 2026)  
**Scope:** Prompt engineering changes in `src/llm/mod.rs`  
**Status:** Planned

---

## Background

QA review of generated Gherkin scenarios identified four recurring categories of errors.
All four categories are caused by insufficient constraints in the LLM prompt preambles —
the parser, formatter, and pipeline architecture require no structural changes.

The affected constants in `src/llm/mod.rs` are:

| Constant | Role |
|---|---|
| `EXTRACTOR_PREAMBLE` | Instructs the extractor LLM to summarise a single document |
| `GENERATOR_PREAMBLE` | Instructs the generator LLM to produce Gherkin from one summary |
| `REVIEWER_PREAMBLE` | Instructs the reviewer LLM to fix a single Gherkin file |
| `GROUP_EXTRACTOR_PREAMBLE` | Same as extractor but for multi-document groups |
| `GROUP_GENERATOR_PREAMBLE` | Same as generator but for multi-document groups |

---

## Issue 1 — Mismatch with Product (Incorrect or Contradictory Behavior)

### Description
Scenarios assert rules, validations, UI behavior, or outcomes that directly contradict
the product service document, or they promote setup/configuration rules into runtime behavior.

### Confirmed Examples
| Scenario | Incorrect Statement | Why |
|---|---|---|
| S747 – Create Premises | `Registration Level` and `Contract hyperlink` asserted as mandatory fields | These fields do not appear on the Create Premises dialog |
| S742 – Import Geographic Locations | `Drawing type`, `Sunken`, `Comment` asserted as mandatory | All three are explicitly optional in the service document |
| S753 – Create Meter and Register | Deletion blocked when meter has an active register | The document only blocks deletion when child assets have *other* parent relations |

### Root Cause
- The extractor does not record field optionality, so the generator cannot distinguish mandatory from optional fields.
- No "document fidelity" rule prohibits the generator from inventing fields or strengthening constraints.

### Planned Changes

#### `EXTRACTOR_PREAMBLE`
Add a new **OPTIONALITY** output section:
- For every field on a dialog or form, record whether the document explicitly marks it **Mandatory (M)** or **Optional (O)**.
- If the document does not state a field is required, classify it as Optional.
- Fields that are only present in FactBoxes, Consumers, or downstream documents must be listed separately with their classification.

#### `GENERATOR_PREAMBLE`
Add a **DOCUMENT FIDELITY** rule:
- Only generate steps that are directly grounded in the document. Never assume, infer, or invent fields, buttons, UI page names, or business rules not explicitly stated.

Add an **OPTIONALITY** rule:
- A field the document marks optional must never appear in a step that asserts it is required or mandatory.
- Do not mix optional and mandatory fields in the same assertion.

#### `REVIEWER_PREAMBLE`
Add a **DOCUMENT FIDELITY CHECK**:
- If a step references a field, page, or rule that cannot be traced to the source document, remove or flag it.

Add an **OPTIONALITY CHECK**:
- If a creation or validation step asserts a field as mandatory when it is documented as optional, rewrite or remove the step.

---

## Issue 2 — Scope Mismatch (Wrong Lifecycle Phase)

### Description
Rules that apply to editing, category-change, or setup phases are incorrectly asserted
during creation, or vice versa.

### Confirmed Examples
| Scenario | Incorrect Statement | Why |
|---|---|---|
| S747 – Create Premises | Open inspection validation asserted at Creation | Document scopes this rule to Category-change only |
| S555 – Create Case | Case Category asserted as changeable post-creation | Document shows Case Category is read-only after creation |

### Root Cause
Rules 8–10 in the current `GENERATOR_PREAMBLE` exist but are insufficiently enforced because
the extractor does not require source-text evidence for lifecycle phase tags, giving the
generator weak or missing signal.

### Planned Changes

#### `EXTRACTOR_PREAMBLE`
Strengthen the `LIFECYCLE_PHASES` rule:
- Require explicit source-text evidence for every lifecycle phase tag assigned to a rule.
- Add: *"If a rule's lifecycle phase is ambiguous, do NOT assume Creation — leave it untagged rather than guess."*

#### `GENERATOR_PREAMBLE`
Harden Rules 9–10 from guidance to hard prohibition:
- *"If no lifecycle phase is stated for a rule at Creation, do NOT place it in a Creation scenario."*
- *"A rule documented only for Category-change or Edit must not appear in a [Creation] scenario under any circumstances."*

#### `REVIEWER_PREAMBLE`
Expand the `LIFECYCLE PHASE CHECK` with the most common failure mode:
- *"A change/edit rule placed in a [Creation] scenario must be moved to the correct phase scenario or removed."*

---

## Issue 3 — Incorrect Quantification or Cardinality

### Description
Scenarios assert wrong minimums, maximums, or exact counts that contradict the product specification.

### Confirmed Examples
| Scenario | Incorrect Statement | Why |
|---|---|---|
| D028 – XML Disconnection Notification | `LinkedTurnOffOns` asserted as `1..∞` | Document defines it as `0..n` (zero or more) |
| S753 – Create Meter and Register | "3 meters created, 2 serial numbers ignored" hardcoded | Document specifies partial creation with a warning; all 5 are created, a warning is issued |

### Root Cause
No cardinality guidance exists anywhere in the current pipeline. The generator freely
converts `0..n` to `1..∞` and invents specific numeric outcomes.

### Planned Changes

#### `EXTRACTOR_PREAMBLE` and `GROUP_EXTRACTOR_PREAMBLE`
Add a new **CARDINALITY** rule and output section:
- For every repeating element or relationship endpoint, capture the exact multiplicity from the document (e.g., `0..n`, `1..∞`, `exactly N`).
- Reproduce the document's own notation verbatim — do not normalise or reinterpret it.

#### `GENERATOR_PREAMBLE` and `GROUP_GENERATOR_PREAMBLE`
Add a **CARDINALITY FIDELITY** rule:
- Reproduce document cardinality exactly. Do NOT change `0..n` to `1..∞`.
- Do NOT hardcode a specific count (e.g., "3 are created") when the document describes partial processing with a warning. In that case, assert the warning message and all-items-attempted behavior instead.

#### `REVIEWER_PREAMBLE`
Add a **CARDINALITY CHECK** rule:
- If a step changes `0..n` to `1..∞` or hardcodes a specific count not stated in the document, correct it to match the documented cardinality.

---

## Issue 4 — Invented or Non-Existent Concepts

### Description
Scenarios introduce fields, navigation targets, UI behavior, or validation constraints
that do not exist anywhere in the product service documentation.

### Confirmed Examples
| Scenario | Incorrect Statement | Why |
|---|---|---|
| S753 – LNA – Create Meter | `Then I am redirected to the meter entity page in Front Office` | Document states Front Office always resolves to Premises. No meter-level Front Office page exists. |
| S842 – Manage Serial ID | `And no other External ID of the same type is marked 'In use'` asserted universally | This validation only applies when toggling the 'In use' checkbox; the document does not block general External ID value updates |

### Root Cause
The generator infers plausible-sounding UI behavior and generalizes conditional rules
beyond the narrow context the document actually defines them in.

### Planned Changes

#### `GENERATOR_PREAMBLE`
Expand the **DOCUMENT FIDELITY** rule (added in Issue 1) to also cover:
- *"Do not infer navigation targets, page names, or redirect behavior unless the document explicitly names the destination."*
- *"Do not generalize a validation rule that the document scopes to a specific action or trigger (e.g., 'applies only when toggling X checkbox') into a universal assertion."*

#### `REVIEWER_PREAMBLE`
Add a **SCOPE GENERALIZATION CHECK**:
- If a step asserts a validation in all cases but the source document scopes it to a specific trigger or condition, narrow the step to match the documented scope.
- If a navigation step references a page or entity for which no navigation target is documented, remove or rewrite it.

---

## Change Summary

### Files to Modify

**`src/llm/mod.rs`** — all changes are to the prompt constant strings only.

| Constant | New Rules Added |
|---|---|
| `EXTRACTOR_PREAMBLE` | OPTIONALITY section; CARDINALITY section; strengthened LIFECYCLE_PHASES evidence requirement |
| `GENERATOR_PREAMBLE` | DOCUMENT FIDELITY rule; OPTIONALITY rule; CARDINALITY FIDELITY rule; hardened lifecycle prohibition |
| `REVIEWER_PREAMBLE` | DOCUMENT FIDELITY CHECK; OPTIONALITY CHECK; CARDINALITY CHECK; SCOPE GENERALIZATION CHECK; expanded LIFECYCLE PHASE CHECK |
| `GROUP_EXTRACTOR_PREAMBLE` | Mirror of EXTRACTOR changes |
| `GROUP_GENERATOR_PREAMBLE` | Mirror of GENERATOR changes |

### Files Not Changed
- `src/gherkin.rs` — parser and formatter are correct
- `src/context.rs` — context accumulation is unaffected
- `src/session.rs` — session persistence is unaffected
- All pipeline orchestration code

---

## Implementation Steps

1. **Update `EXTRACTOR_PREAMBLE`** — add rules 12 (OPTIONALITY) and 13 (CARDINALITY); strengthen rule 9 (LIFECYCLE_PHASES)
2. **Update `GENERATOR_PREAMBLE`** — add rules 11 (DOCUMENT FIDELITY), 12 (OPTIONALITY), 13 (CARDINALITY FIDELITY); harden rules 9–10
3. **Update `REVIEWER_PREAMBLE`** — add rules 10 (DOCUMENT FIDELITY CHECK), 11 (OPTIONALITY CHECK), 12 (CARDINALITY CHECK), 13 (SCOPE GENERALIZATION CHECK); expand rule 9
4. **Mirror to `GROUP_EXTRACTOR_PREAMBLE`** — apply same OPTIONALITY and CARDINALITY additions
5. **Mirror to `GROUP_GENERATOR_PREAMBLE`** — apply same DOCUMENT FIDELITY, OPTIONALITY, and CARDINALITY FIDELITY additions
6. **Regression test** — re-run the four failing scenario families (S747, S742, S753, S555, D028, S842) and verify the new output no longer exhibits any of the four issue patterns
