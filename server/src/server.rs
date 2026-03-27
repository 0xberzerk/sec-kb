use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
    ServerHandler,
};
use serde::Deserialize;

use kb_core::types::{CurationContext, CurationStatus, FeedbackItem, Pipeline};
use kb_core::KnowledgeBase;

#[derive(Clone)]
pub struct KbServer {
    tool_router: ToolRouter<Self>,
    kb: Arc<KnowledgeBase>,
}

// -- Tool parameter schemas --

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IngestParams {
    /// Free-text search keywords used in the original Solodit query.
    pub keywords: String,
    #[serde(default)]
    pub impact: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub protocol_categories: Vec<String>,
    #[serde(default)]
    pub min_quality: Option<u8>,
    pub findings: Vec<IngestFindingParam>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IngestFindingParam {
    pub slug: String,
    pub title: String,
    pub impact: String,
    #[serde(default)]
    pub quality_score: f64,
    #[serde(default)]
    pub firm: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CurateParams {
    #[serde(default)]
    pub codebase_keywords: Vec<String>,
    #[serde(default)]
    pub integration_types: Vec<String>,
    #[serde(default)]
    pub protocol_categories: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QueryParams {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
    #[serde(default = "default_true")]
    pub exclude_noise: bool,
    /// Pipeline requesting the query. "design_review" or "audit_sandbox".
    /// When set, entries with context at the matching abstraction level rank higher.
    #[serde(default)]
    pub pipeline: Option<String>,
}

fn default_max_entries() -> usize {
    50
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetCurationParams {
    pub entry_id: String,
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackParams {
    pub items: Vec<FeedbackItemParam>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FeedbackItemParam {
    pub entry_id: String,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// Which pipeline is providing this feedback. "design_review" or "audit_sandbox".
    #[serde(default)]
    pub pipeline: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImportSeedParams {
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExportParams {
    pub domain: String,
    #[serde(default = "default_export_description")]
    pub description: String,
    pub output_path: String,
}

fn default_export_description() -> String {
    "Exported curated entries".to_string()
}

// -- Tool implementations --

#[tool_router(router = tool_router)]
impl KbServer {
    pub fn new(kb: KnowledgeBase) -> Self {
        Self {
            tool_router: Self::tool_router(),
            kb: Arc::new(kb),
        }
    }

    #[tool(description = "Ingest Solodit search results into the Knowledge Base raw cache. Computes a fingerprint from query params, skips if cache is fresh, and filters out non-HIGH/MEDIUM findings. Returns the number of entries ingested.")]
    async fn kb_ingest(&self, Parameters(params): Parameters<IngestParams>) -> String {
        let query_params = kb_core::RawQueryParams {
            keywords: params.keywords,
            impact: params.impact,
            tags: params.tags,
            protocol_categories: params.protocol_categories,
            min_quality: params.min_quality,
        };

        let findings: Vec<kb_core::IngestFinding> = params
            .findings
            .into_iter()
            .map(|f| kb_core::IngestFinding {
                slug: f.slug,
                title: f.title,
                impact: f.impact,
                quality_score: f.quality_score,
                firm: f.firm,
                protocol: f.protocol,
                tags: f.tags,
                category: f.category,
                summary: f.summary,
                content: f.content,
            })
            .collect();

        match self.kb.ingest(&query_params, findings) {
            Ok(count) => format!("{{\"ingested\": {}}}", count),
            Err(e) => format!("{{\"error\": \"ingest_failed\", \"message\": \"{}\"}}", e),
        }
    }

    #[tool(description = "Run a curation pass over all raw cache and seed entries. Deduplicates, scores by quality and impact, and writes curated severity files. Preserves existing curation status for previously curated entries.")]
    async fn kb_curate(&self, Parameters(params): Parameters<CurateParams>) -> String {
        let context = CurationContext {
            codebase_keywords: params.codebase_keywords,
            integration_types: params.integration_types,
            protocol_categories: params.protocol_categories,
        };

        match self.kb.curate(&context) {
            Ok(stats) => format!(
                "{{\"total_processed\": {}, \"high_count\": {}, \"medium_count\": {}, \"noise_skipped\": {}}}",
                stats.total_processed, stats.high_count, stats.medium_count, stats.noise_skipped
            ),
            Err(e) => format!("{{\"error\": \"curate_failed\", \"message\": \"{}\"}}", e),
        }
    }

    #[tool(description = "Query the curated Knowledge Base for agent consumption. Returns entries ordered by severity (HIGH before MEDIUM) then curation rank. Filter by tags, categories, and keywords. Optionally set pipeline ('design_review' or 'audit_sandbox') to boost entries with context at the matching abstraction level.")]
    async fn kb_query(&self, Parameters(params): Parameters<QueryParams>) -> String {
        let pipeline = params.pipeline.as_deref().and_then(parse_pipeline);

        let query = kb_core::types::KbQuery {
            tags: params.tags,
            categories: params.categories,
            keywords: params.keywords,
            max_entries: params.max_entries,
            exclude_noise: params.exclude_noise,
            pipeline,
        };

        match self.kb.query(&query) {
            Ok(result) => {
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| {
                    format!("{{\"error\": \"serialize_failed\", \"message\": \"{}\"}}", e)
                })
            }
            Err(e) => format!("{{\"error\": \"query_failed\", \"message\": \"{}\"}}", e),
        }
    }

    #[tool(description = "Update curation status for a single Knowledge Base entry. Status can be: 'unreviewed', 'useful', 'noise', or 'critical'. Use after auditor reviews findings.")]
    async fn kb_set_curation(
        &self,
        Parameters(params): Parameters<SetCurationParams>,
    ) -> String {
        let status = match parse_curation_status(&params.status) {
            Some(s) => s,
            None => {
                return format!(
                    "{{\"error\": \"invalid_status\", \"message\": \"must be one of: unreviewed, useful, noise, critical\"}}"
                )
            }
        };

        match self.kb.set_curation(&params.entry_id, status, params.notes) {
            Ok(true) => "{\"updated\": true}".to_string(),
            Ok(false) => format!(
                "{{\"updated\": false, \"message\": \"entry '{}' not found\"}}",
                params.entry_id
            ),
            Err(e) => format!(
                "{{\"error\": \"set_curation_failed\", \"message\": \"{}\"}}",
                e
            ),
        }
    }

    #[tool(description = "Apply bulk feedback from auditor review. Maps audit actions to curation status: confirmed → useful, false-positive → noise, escalate → critical. Optionally set pipeline per item to track which pipeline confirmed the entry.")]
    async fn kb_apply_feedback(
        &self,
        Parameters(params): Parameters<FeedbackParams>,
    ) -> String {
        let mut items = Vec::new();
        for item in params.items {
            let status = match parse_curation_status(&item.status) {
                Some(s) => s,
                None => {
                    return format!(
                        "{{\"error\": \"invalid_status\", \"message\": \"'{}' for entry '{}' is not valid\"}}",
                        item.status, item.entry_id
                    )
                }
            };
            let pipeline = item.pipeline.as_deref().and_then(parse_pipeline);
            items.push(FeedbackItem {
                entry_id: item.entry_id,
                new_status: status,
                reason: item.reason,
                pipeline,
            });
        }

        match self.kb.apply_feedback(&items) {
            Ok(count) => format!("{{\"updated\": {}}}", count),
            Err(e) => format!(
                "{{\"error\": \"feedback_failed\", \"message\": \"{}\"}}",
                e
            ),
        }
    }

    #[tool(description = "Export curated Knowledge Base entries as a portable seed file. Only exports entries with 'useful' or 'critical' curation status. The exported file can be imported into a future review via kb_import_seed.")]
    async fn kb_export(
        &self,
        Parameters(params): Parameters<ExportParams>,
    ) -> String {
        let output_path = std::path::Path::new(&params.output_path);
        match self
            .kb
            .export_curated(&params.domain, &params.description, output_path)
        {
            Ok(stats) => format!("{{\"exported\": {}}}", stats.exported),
            Err(e) => format!(
                "{{\"error\": \"export_failed\", \"message\": \"{}\"}}",
                e
            ),
        }
    }

    #[tool(description = "Import a seed file into the Knowledge Base. Seed files contain curated known vulnerabilities, design anti-patterns, and bookmarked findings. Provide the absolute path to the seed JSON file.")]
    async fn kb_import_seed(
        &self,
        Parameters(params): Parameters<ImportSeedParams>,
    ) -> String {
        let path = std::path::Path::new(&params.path);
        match self.kb.import_seed_file(path) {
            Ok(count) => format!("{{\"imported\": {}}}", count),
            Err(e) => format!(
                "{{\"error\": \"import_failed\", \"message\": \"{}\"}}",
                e
            ),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KbServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Shared Knowledge Base for security review pipelines (design-review + audit-sandbox). \
             Curated vulnerability index between Solodit and the agent pipelines. \
             Ingest findings, run curation passes, and query for agent consumption. \
             Supports pipeline-aware queries to rank entries by abstraction level.",
        )
    }
}

fn parse_curation_status(s: &str) -> Option<CurationStatus> {
    match s.to_lowercase().as_str() {
        "unreviewed" => Some(CurationStatus::Unreviewed),
        "useful" => Some(CurationStatus::Useful),
        "noise" => Some(CurationStatus::Noise),
        "critical" => Some(CurationStatus::Critical),
        _ => None,
    }
}

fn parse_pipeline(s: &str) -> Option<Pipeline> {
    match s.to_lowercase().as_str() {
        "design_review" => Some(Pipeline::DesignReview),
        "audit_sandbox" => Some(Pipeline::AuditSandbox),
        _ => None,
    }
}
