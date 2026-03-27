pub mod curate;
pub mod fingerprint;
pub mod query;
pub mod raw;
pub mod store;
pub mod types;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::curate::Curator;
use crate::query::QueryEngine;
use crate::raw::RawCache;
use crate::store::Store;

// Re-exports for convenience
pub use crate::raw::IngestFinding;
pub use crate::types::{
    CurationContext, CurationStats, CurationStatus, EntrySource, ExportStats, FeedbackItem,
    Impact, KbEntry, KbQuery, KbQueryResult, Pipeline, RawQueryParams, SeedLevel,
};

/// Knowledge Base — local curated vulnerability index.
///
/// Shared between design-review and audit-sandbox pipelines.
/// Three layers: raw cache → curated index → agent consumption.
pub struct KnowledgeBase {
    store: Store,
}

impl KnowledgeBase {
    /// Initialize KB with a directory path. Creates subdirectories if needed.
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        let store = Store::new(base_dir).context("initializing KB store")?;
        Ok(Self { store })
    }

    // -- Layer 1: Raw Cache --

    pub fn ingest(
        &self,
        params: &RawQueryParams,
        findings: Vec<IngestFinding>,
    ) -> Result<usize> {
        RawCache::new(&self.store).ingest(params, findings)
    }

    pub fn is_cached(&self, params: &RawQueryParams) -> Result<bool> {
        RawCache::new(&self.store).is_cached(params)
    }

    pub fn invalidate(&self, params: &RawQueryParams) -> Result<()> {
        RawCache::new(&self.store).invalidate(params)
    }

    pub fn evict_expired(&self) -> Result<usize> {
        RawCache::new(&self.store).evict_expired()
    }

    // -- Layer 2: Curation --

    pub fn curate(&self, context: &CurationContext) -> Result<CurationStats> {
        let raw = RawCache::new(&self.store);
        let curator = Curator::new(&self.store);

        let mut all_entries = raw.all_entries()?;
        let seed_entries = self.collect_seed_entries()?;
        all_entries.extend(seed_entries);

        curator.curate(all_entries, context)
    }

    pub fn set_curation(
        &self,
        entry_id: &str,
        status: CurationStatus,
        notes: Option<String>,
    ) -> Result<bool> {
        Curator::new(&self.store).set_curation(entry_id, status, notes)
    }

    pub fn apply_feedback(&self, feedback: &[FeedbackItem]) -> Result<usize> {
        Curator::new(&self.store).apply_feedback(feedback)
    }

    // -- Layer 3: Agent Consumption --

    pub fn query(&self, q: &KbQuery) -> Result<KbQueryResult> {
        QueryEngine::new(&self.store).query(q)
    }

    // -- Seeds --

    pub fn import_seed_file(&self, path: &Path) -> Result<usize> {
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("reading seed file {}", path.display()))?;
        let seed: crate::types::SeedFile = serde_json::from_str(&data)
            .with_context(|| format!("parsing seed file {}", path.display()))?;
        let count = seed.entries.len();
        self.store.write_seed(&seed)?;
        Ok(count)
    }

    pub fn list_seeds(&self) -> Result<Vec<PathBuf>> {
        self.store.list_seed_paths()
    }

    // -- Export --

    pub fn export_curated(&self, domain: &str, description: &str, output_path: &Path) -> Result<ExportStats> {
        let mut entries = Vec::new();

        for impact in &[Impact::High, Impact::Medium] {
            if let Some(file) = self.store.read_curated(impact)? {
                for entry in file.entries {
                    if entry.curation == CurationStatus::Useful
                        || entry.curation == CurationStatus::Critical
                    {
                        entries.push(entry);
                    }
                }
            }
        }

        let count = entries.len();

        let seed = crate::types::SeedFile {
            domain: domain.to_string(),
            description: description.to_string(),
            level: SeedLevel::Both,
            entries,
        };

        let json = serde_json::to_string_pretty(&seed)
            .context("serializing export seed file")?;
        std::fs::write(output_path, json)
            .with_context(|| format!("writing export to {}", output_path.display()))?;

        Ok(ExportStats { exported: count })
    }

    // -- Internal --

    fn collect_seed_entries(&self) -> Result<Vec<KbEntry>> {
        let seeds = self.store.read_seeds()?;
        let entries = seeds
            .into_iter()
            .flat_map(|sf| sf.entries)
            .map(|mut e| {
                match e.source {
                    EntrySource::Solodit | EntrySource::Manual => {}
                    EntrySource::Seed => {
                        if !e.id.starts_with("seed:") {
                            e.id = format!("seed:{}", e.slug);
                        }
                    }
                }
                e
            })
            .collect();
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::TempDir;

    fn temp_kb() -> (TempDir, KnowledgeBase) {
        let tmp = TempDir::new().unwrap();
        let kb = KnowledgeBase::new(tmp.path().to_path_buf()).unwrap();
        (tmp, kb)
    }

    fn sample_params(keywords: &str) -> RawQueryParams {
        RawQueryParams {
            keywords: keywords.to_string(),
            impact: vec!["HIGH".to_string(), "MEDIUM".to_string()],
            tags: vec![],
            protocol_categories: vec![],
            min_quality: None,
        }
    }

    fn sample_finding(slug: &str, impact: &str, tags: &[&str], category: &str) -> IngestFinding {
        IngestFinding {
            slug: slug.to_string(),
            title: format!("Finding: {}", slug),
            impact: impact.to_string(),
            quality_score: 4.0,
            firm: "Firm".to_string(),
            protocol: "Proto".to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            category: category.to_string(),
            summary: Some(format!("Summary for {}", slug)),
            content: None,
        }
    }

    #[test]
    fn e2e_ingest_curate_query() {
        let (_tmp, kb) = temp_kb();
        kb.ingest(&sample_params("reentrancy"), vec![
            sample_finding("reent-1", "HIGH", &["Reentrancy"], "Lending"),
            sample_finding("reent-2", "MEDIUM", &["Reentrancy"], "Lending"),
        ]).unwrap();

        let stats = kb.curate(&CurationContext::default()).unwrap();
        assert_eq!(stats.total_processed, 2);

        let result = kb.query(&KbQuery { max_entries: 100, ..Default::default() }).unwrap();
        assert_eq!(result.entries.len(), 2);
    }

    #[test]
    fn e2e_feedback_with_pipeline() {
        let (_tmp, kb) = temp_kb();
        kb.ingest(&sample_params("test"), vec![
            sample_finding("confirmed-bug", "HIGH", &[], ""),
        ]).unwrap();
        kb.curate(&CurationContext::default()).unwrap();

        let feedback = vec![FeedbackItem {
            entry_id: "solodit:confirmed-bug".to_string(),
            new_status: CurationStatus::Useful,
            reason: Some("valid".to_string()),
            pipeline: Some(Pipeline::AuditSandbox),
        }];
        kb.apply_feedback(&feedback).unwrap();

        let result = kb.query(&KbQuery { max_entries: 100, ..Default::default() }).unwrap();
        let entry = &result.entries[0];
        assert_eq!(entry.curation, CurationStatus::Useful);
        assert_eq!(entry.confirmed_by.len(), 1);
        assert_eq!(entry.confirmed_by[0].pipeline, Pipeline::AuditSandbox);
    }

    #[test]
    fn e2e_seeds_merge_with_solodit() {
        let (tmp, kb) = temp_kb();

        let seed_json = format!(
            r#"{{"domain": "Yield", "description": "test", "level": "design", "entries": [{{
                "id": "seed:inflation-attack",
                "slug": "inflation-attack",
                "title": "Seed: inflation-attack",
                "impact": "HIGH",
                "quality_score": 5.0,
                "tags": [],
                "category": "Yield",
                "summary": "Seed summary",
                "content": null,
                "source": "seed",
                "curation": "critical",
                "relevance_score": 1.0,
                "confidence": 1.0,
                "ingested_at": "{}",
                "last_curated_at": null,
                "auditor_notes": null,
                "confirmed_by": []
            }}]}}"#,
            Utc::now().to_rfc3339()
        );
        let seed_path = tmp.path().join("external-seed.json");
        std::fs::write(&seed_path, &seed_json).unwrap();
        kb.import_seed_file(&seed_path).unwrap();

        kb.ingest(&sample_params("vault"), vec![
            sample_finding("vault-bug", "HIGH", &["ERC4626"], "Yield"),
        ]).unwrap();

        let stats = kb.curate(&CurationContext::default()).unwrap();
        assert_eq!(stats.high_count, 2);
    }
}
