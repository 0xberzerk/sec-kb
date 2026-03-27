use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Impact level — only HIGH and MEDIUM are stored (severity guardrail).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum Impact {
    High,
    Medium,
}

impl Impact {
    /// Numeric rank for sorting (higher = more severe).
    pub fn rank(&self) -> u8 {
        match self {
            Impact::High => 2,
            Impact::Medium => 1,
        }
    }
}

/// Curation status — set by auditor or auto-curated by Architect.
///
/// "Critical" here means "high-signal, always surface" — it is NOT a severity
/// level. Solodit only has HIGH/MEDIUM impact; this is a local relevance flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CurationStatus {
    /// Fresh from API, not yet seen by a human or agent.
    Unreviewed,
    /// Confirmed relevant by auditor or agent feedback.
    Useful,
    /// Irrelevant to this domain/pattern, deprioritized in future queries.
    Noise,
    /// High-signal — should always surface for this integration type.
    Critical,
}

impl Default for CurationStatus {
    fn default() -> Self {
        CurationStatus::Unreviewed
    }
}

impl CurationStatus {
    /// Numeric rank for sorting within a severity bucket.
    /// Higher = surfaces first.
    pub fn rank(&self) -> u8 {
        match self {
            CurationStatus::Critical => 3,
            CurationStatus::Useful => 2,
            CurationStatus::Unreviewed => 1,
            CurationStatus::Noise => 0,
        }
    }
}

/// How this entry entered the KB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntrySource {
    /// Fetched via Solodit MCP/API.
    Solodit,
    /// Auditor pre-seeded (known bugs, war stories).
    Seed,
    /// Added manually during an audit or review.
    Manual,
}

/// Which pipeline produced or confirmed an entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Pipeline {
    DesignReview,
    AuditSandbox,
}

/// Abstraction level for seed files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SeedLevel {
    Code,
    Design,
    Both,
}

impl Default for SeedLevel {
    fn default() -> Self {
        SeedLevel::Code
    }
}

// ---------------------------------------------------------------------------
// Abstraction context
// ---------------------------------------------------------------------------

/// What to look for at a specific abstraction level (design or code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractionContext {
    /// Concrete indicators at this abstraction level.
    pub indicators: Vec<String>,
    /// How this pattern manifests at this level.
    #[serde(default)]
    pub description: Option<String>,
}

/// Tracks which pipeline confirmed this entry and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfirmation {
    pub pipeline: Pipeline,
    pub confirmed_at: DateTime<Utc>,
    /// E.g. "confirmed in vault-design review" or "confirmed in NFT-dealers audit"
    #[serde(default)]
    pub context: Option<String>,
}

// ---------------------------------------------------------------------------
// Core entry
// ---------------------------------------------------------------------------

/// A single finding stored in the KB.
///
/// Used across all three layers (raw, curated, seeds) — same struct, different
/// lifecycle. Raw entries have default scores; curated entries are scored and
/// tagged; seeds arrive pre-curated by the auditor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbEntry {
    // -- Identity --
    /// Unique within KB: "{source}:{slug}" for Solodit, "{source}:{hash}" for seeds.
    pub id: String,
    pub slug: String,
    pub title: String,
    pub impact: Impact,
    pub quality_score: f64,
    /// Audit firm name from Solodit. Empty for non-Solodit entries.
    #[serde(default)]
    pub firm: String,
    /// Protocol audited. Empty for non-Solodit entries.
    #[serde(default)]
    pub protocol: String,
    /// E.g. ["Reentrancy", "ERC4626"] — raw from Solodit, no mapping.
    pub tags: Vec<String>,
    /// E.g. "Lending" — raw from Solodit, no mapping.
    pub category: String,
    pub summary: Option<String>,
    pub content: Option<String>,

    // -- Abstraction-level context --
    /// What to look for in design specs — for the design-review pipeline.
    #[serde(default)]
    pub design_context: Option<AbstractionContext>,
    /// What to look for in source code — for the audit-sandbox pipeline.
    #[serde(default)]
    pub code_context: Option<AbstractionContext>,

    // -- KB metadata --
    pub source: EntrySource,
    #[serde(default)]
    pub curation: CurationStatus,
    /// 0.0..1.0 — computed during curation. Default 0.0 for raw entries.
    #[serde(default)]
    pub relevance_score: f64,
    /// 0.0..1.0 — how much agents should trust this reference.
    #[serde(default)]
    pub confidence: f64,
    pub ingested_at: DateTime<Utc>,
    pub last_curated_at: Option<DateTime<Utc>>,
    pub auditor_notes: Option<String>,

    // -- Cross-pipeline confirmation tracking --
    /// Which pipelines have confirmed/used this entry.
    #[serde(default)]
    pub confirmed_by: Vec<PipelineConfirmation>,
}

// ---------------------------------------------------------------------------
// Raw cache envelope
// ---------------------------------------------------------------------------

/// Wraps a set of entries with query metadata for the raw cache layer.
/// One file per unique query fingerprint in `data/raw/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCacheEnvelope {
    pub fingerprint: String,
    pub query_params: RawQueryParams,
    pub fetched_at: DateTime<Utc>,
    pub ttl_secs: u64,
    pub entries: Vec<KbEntry>,
}

impl RawCacheEnvelope {
    /// Whether this cache entry has expired.
    pub fn is_expired(&self) -> bool {
        let ttl = chrono::Duration::seconds(self.ttl_secs as i64);
        Utc::now() > self.fetched_at + ttl
    }
}

/// The query parameters that produced a raw cache entry.
/// Stored for traceability — also used to compute the fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawQueryParams {
    pub keywords: String,
    pub impact: Vec<String>,
    pub tags: Vec<String>,
    pub protocol_categories: Vec<String>,
    pub min_quality: Option<u8>,
}

// ---------------------------------------------------------------------------
// Curated file envelope
// ---------------------------------------------------------------------------

/// Wrapper for a curated severity bucket file (high.json, medium.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedFile {
    pub impact: Impact,
    pub last_curated_at: DateTime<Utc>,
    pub entries: Vec<KbEntry>,
}

// ---------------------------------------------------------------------------
// Seed file
// ---------------------------------------------------------------------------

/// A seed file dropped by the auditor in `data/seeds/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedFile {
    pub domain: String,
    pub description: String,
    /// Which abstraction level(s) this seed targets.
    /// Default: Code (backward compat with existing audit-sandbox seeds).
    #[serde(default)]
    pub level: SeedLevel,
    pub entries: Vec<KbEntry>,
}

// ---------------------------------------------------------------------------
// Query types (Layer 3 — agent consumption)
// ---------------------------------------------------------------------------

/// Request from an agent to query the curated index.
#[derive(Debug, Clone)]
pub struct KbQuery {
    /// Filter by tags (e.g. ["Reentrancy", "ERC4626"]). Empty = no filter.
    pub tags: Vec<String>,
    /// Filter by categories (e.g. ["Lending"]). Empty = no filter.
    pub categories: Vec<String>,
    /// Keyword search against title/summary. Empty = no filter.
    pub keywords: Vec<String>,
    /// Max entries to return (context budget).
    pub max_entries: usize,
    /// Exclude entries with curation status "noise". Default true.
    pub exclude_noise: bool,
    /// Pipeline requesting the query. When set, entries with context at
    /// the matching abstraction level rank higher (soft boost, not hard filter).
    pub pipeline: Option<Pipeline>,
}

impl Default for KbQuery {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            categories: Vec::new(),
            keywords: Vec::new(),
            max_entries: 50,
            exclude_noise: true,
            pipeline: None,
        }
    }
}

/// Response from a KB query — what agents consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KbQueryResult {
    /// Ordered: HIGH before MEDIUM, then by curation rank within each.
    pub entries: Vec<KbEntry>,
    /// Total matching entries before truncation.
    pub total_available: usize,
    /// Whether the result was truncated to fit the budget.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Feedback types
// ---------------------------------------------------------------------------

/// A single feedback item from auditor review.
pub struct FeedbackItem {
    pub entry_id: String,
    pub new_status: CurationStatus,
    pub reason: Option<String>,
    /// Which pipeline is providing this feedback.
    pub pipeline: Option<Pipeline>,
}

// ---------------------------------------------------------------------------
// Curation context
// ---------------------------------------------------------------------------

/// Context provided by the Architect for relevance scoring.
/// Boosts entries matching codebase integrations, categories, and keywords.
#[derive(Debug, Clone, Default)]
pub struct CurationContext {
    /// Contract names, function names, identifiers from the source.
    pub codebase_keywords: Vec<String>,
    /// From @audit-integration or @design-integration tags.
    pub integration_types: Vec<String>,
    /// From @audit/@design tags or Architect detection.
    pub protocol_categories: Vec<String>,
}

/// Stats returned after a curation pass.
#[derive(Debug, Clone)]
pub struct CurationStats {
    pub total_processed: usize,
    pub high_count: usize,
    pub medium_count: usize,
    pub noise_skipped: usize,
}

/// Stats returned after an export.
#[derive(Debug, Clone)]
pub struct ExportStats {
    pub exported: usize,
}
