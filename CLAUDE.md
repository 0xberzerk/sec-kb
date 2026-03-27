# CLAUDE.md

## Project Overview

**sec-kb** — Shared Knowledge Base for security review pipelines. Curated vulnerability index consumed by both `design-review` (pre-code design analysis) and `audit-sandbox` (Solidity code audit).

Patterns are the same across both pipelines — the abstraction level is the difference. Each KB entry can have a `design_context` (what to look for in specs) and/or a `code_context` (what to look for in source code).

## Architecture

### Three-Layer Design

1. **Raw Cache** (`data/raw/`) — TTL-based Solodit API cache. Fingerprint-deduplicated, expires per query.
2. **Curated Index** (`data/curated/`) — Scored, deduplicated, severity-bucketed (high.json, medium.json). Human curation status preserved across re-runs.
3. **Seeds** (`data/seeds/`) — Pre-curated entries organized by abstraction level:
   - `code/` — Code-level patterns (from audit-sandbox)
   - `design/` — Design-level patterns (from design-review)
   - `shared/` — Patterns serving both levels

### Entry Schema

Key fields per `KbEntry`:
- **Identity:** id, slug, title, impact (HIGH/MEDIUM only), quality_score
- **Content:** summary, content, tags[], category
- **Abstraction context:** `design_context` (indicators + description), `code_context` (indicators + description) — both optional
- **Metadata:** source (Solodit/Seed/Manual), curation (Unreviewed/Useful/Noise/Critical), relevance_score, confidence
- **Cross-pipeline tracking:** `confirmed_by[]` — records which pipeline(s) confirmed this entry

### Severity Guardrail

Only HIGH and MEDIUM impact entries are stored. "Critical" is a curation status (high-signal, always surface), NOT a severity level.

### Pipeline-Aware Queries

The `kb_query` tool accepts an optional `pipeline` parameter ("design_review" or "audit_sandbox"). When set, entries with context at the matching abstraction level rank higher (soft boost, not hard filter). Entries without matching context are still returned.

## Workspace Structure

- **kb-core/** — Library crate: types, store, curate, query, raw, fingerprint
- **server/** — MCP server binary: tool definitions, stdio transport

## MCP Tools

| Tool | Purpose |
|------|---------|
| `kb_ingest` | Ingest Solodit search results into raw cache |
| `kb_curate` | Run curation pass (score, dedup, bucket) |
| `kb_query` | Query curated index for agent consumption (pipeline-aware) |
| `kb_set_curation` | Update single entry curation status |
| `kb_apply_feedback` | Bulk feedback from auditor review (pipeline-aware) |
| `kb_import_seed` | Import seed file |
| `kb_export` | Export curated entries as portable seed file |

## Consumer Pipelines

- **audit-sandbox** (`../audit-sandbox/`) — `.mcp.json` points to `../sec-kb/run.sh`
- **design-review** (`../design-review/`) — `.mcp.json` points to `../sec-kb/run.sh`

## Key Design Decisions

- **Rust only** — zero-dependency portability across hosts
- **Shared entries, not shared indices** — one curated index, `confirmed_by` tracks pipeline provenance
- **Soft ranking, not hard filtering** — pipeline param boosts relevant entries without excluding others
- **Curation status preserved** — human decisions stick across re-ingestion and re-curation
- **No embeddings/semantic search** — keyword/tag/category filtering is sufficient at current scale
