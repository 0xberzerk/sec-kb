# sec-kb

Shared Knowledge Base for security review pipelines.

Curated vulnerability index consumed by [design-review](../design-review) (pre-code design analysis) and [audit-sandbox](../audit-sandbox) (Solidity code audit). Patterns are the same — the abstraction level is the difference.

## Quick Start

```bash
# Build
cargo build --release

# Run MCP server (stdio transport)
KB_DIR=./data ./server/target/release/knowledge-base

# Or use the wrapper script
./run.sh
```

## Architecture

Three-layer design: raw Solodit cache → curated scored index → agent consumption.

```
data/
├── raw/              # Solodit API cache (TTL-based)
├── curated/          # Scored + bucketed (high.json, medium.json)
└── seeds/            # Pre-curated entries
    ├── code/         # Code-level patterns
    ├── design/       # Design-level patterns
    └── shared/       # Both levels
```

Each entry can have `design_context` (what to look for in specs) and/or `code_context` (what to look for in source). Pipeline-aware queries boost entries with matching context.

## Integration

Both pipelines consume via MCP:

```json
{
  "mcpServers": {
    "knowledge-base": { "command": "../sec-kb/run.sh" }
  }
}
```

## Seeds

Design-level seeds derived from common design flaws (13 patterns across 6 domains):
- Access control — unbounded admin privilege, missing role separation
- Dependencies — single oracle without fallback, implicit trust in external protocols
- Economic design — missing invariants, extraction vectors, incentive misalignment
- State management — incomplete state machine, stuck state recovery
- Temporal — missing rate limiting, ordering assumptions
- Upgrade/migration — unsafe upgrade patterns, no versioning strategy
