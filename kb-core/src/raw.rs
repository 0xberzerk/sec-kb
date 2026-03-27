use anyhow::Result;
use chrono::Utc;

use crate::fingerprint::fingerprint;
use crate::store::Store;
use crate::types::{
    CurationStatus, EntrySource, Impact, KbEntry, RawCacheEnvelope, RawQueryParams,
};

/// Default TTL for raw cache entries.
/// Override with KB_RAW_TTL_SECS env var (e.g., 1800 for 30-min audit sessions).
const DEFAULT_TTL_SECS: u64 = 300;

fn raw_ttl() -> u64 {
    std::env::var("KB_RAW_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_SECS)
}

/// Ingestion input — a single finding as received from the Solodit MCP.
/// Mirror struct with zero coupling to the Solodit crate.
#[derive(Debug, Clone)]
pub struct IngestFinding {
    pub slug: String,
    pub title: String,
    pub impact: String,
    pub quality_score: f64,
    pub firm: String,
    pub protocol: String,
    pub tags: Vec<String>,
    pub category: String,
    pub summary: Option<String>,
    pub content: Option<String>,
}

/// Layer 1 — Raw cache operations.
///
/// Handles ingestion from Solodit search results, TTL-based deduplication,
/// and forced refresh. All data is persisted to `data/raw/`.
pub struct RawCache<'a> {
    store: &'a Store,
}

impl<'a> RawCache<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Ingest findings from a Solodit search into the raw cache.
    pub fn ingest(
        &self,
        params: &RawQueryParams,
        findings: Vec<IngestFinding>,
    ) -> Result<usize> {
        self.ingest_with_ttl(params, findings, raw_ttl())
    }

    /// Ingest with a custom TTL (useful for testing).
    pub fn ingest_with_ttl(
        &self,
        params: &RawQueryParams,
        findings: Vec<IngestFinding>,
        ttl_secs: u64,
    ) -> Result<usize> {
        let fp = fingerprint(params);

        // Check if cache is fresh
        if let Some(existing) = self.store.read_raw(&fp)? {
            if !existing.is_expired() {
                tracing::debug!("raw cache hit for fingerprint {}, skipping ingestion", fp);
                return Ok(0);
            }
        }

        let entries: Vec<KbEntry> = findings
            .into_iter()
            .filter_map(to_kb_entry)
            .collect();

        let count = entries.len();

        let envelope = RawCacheEnvelope {
            fingerprint: fp,
            query_params: params.clone(),
            fetched_at: Utc::now(),
            ttl_secs,
            entries,
        };

        self.store.write_raw(&envelope)?;
        Ok(count)
    }

    /// Check if a query is cached and still fresh.
    pub fn is_cached(&self, params: &RawQueryParams) -> Result<bool> {
        let fp = fingerprint(params);
        match self.store.read_raw(&fp)? {
            Some(env) => Ok(!env.is_expired()),
            None => Ok(false),
        }
    }

    /// Force refresh: delete the cached entry for this fingerprint.
    pub fn invalidate(&self, params: &RawQueryParams) -> Result<()> {
        let fp = fingerprint(params);
        self.store.delete_raw(&fp)
    }

    /// Collect all entries across all raw cache files (expired or not).
    pub fn all_entries(&self) -> Result<Vec<KbEntry>> {
        let envelopes = self.store.list_raw()?;
        let entries = envelopes
            .into_iter()
            .flat_map(|env| env.entries)
            .collect();
        Ok(entries)
    }

    /// Collect entries only from non-expired cache files.
    pub fn fresh_entries(&self) -> Result<Vec<KbEntry>> {
        let envelopes = self.store.list_raw()?;
        let entries = envelopes
            .into_iter()
            .filter(|env| !env.is_expired())
            .flat_map(|env| env.entries)
            .collect();
        Ok(entries)
    }

    /// Evict all expired raw cache files from disk.
    pub fn evict_expired(&self) -> Result<usize> {
        let envelopes = self.store.list_raw()?;
        let mut evicted = 0;
        for env in envelopes {
            if env.is_expired() {
                self.store.delete_raw(&env.fingerprint)?;
                evicted += 1;
            }
        }
        Ok(evicted)
    }
}

/// Convert an ingestion finding to a KbEntry.
/// Returns None if the impact is not HIGH or MEDIUM (severity guardrail).
fn to_kb_entry(f: IngestFinding) -> Option<KbEntry> {
    let impact = match f.impact.to_uppercase().as_str() {
        "HIGH" => Impact::High,
        "MEDIUM" => Impact::Medium,
        _ => {
            tracing::warn!(
                "dropping finding '{}' with unsupported impact '{}'",
                f.slug,
                f.impact
            );
            return None;
        }
    };

    let id = format!("solodit:{}", f.slug);

    Some(KbEntry {
        id,
        slug: f.slug,
        title: f.title,
        impact,
        quality_score: f.quality_score,
        firm: f.firm,
        protocol: f.protocol,
        tags: f.tags,
        category: f.category,
        summary: f.summary,
        content: f.content,
        design_context: None,
        code_context: None,
        source: EntrySource::Solodit,
        curation: CurationStatus::Unreviewed,
        relevance_score: 0.0,
        confidence: 0.0,
        ingested_at: Utc::now(),
        last_curated_at: None,
        auditor_notes: None,
        confirmed_by: vec![],
        contributor: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().to_path_buf()).unwrap();
        (tmp, store)
    }

    fn sample_params() -> RawQueryParams {
        RawQueryParams {
            keywords: "reentrancy".to_string(),
            impact: vec!["HIGH".to_string(), "MEDIUM".to_string()],
            tags: vec!["Reentrancy".to_string()],
            protocol_categories: vec![],
            min_quality: None,
        }
    }

    fn sample_finding(slug: &str, impact: &str) -> IngestFinding {
        IngestFinding {
            slug: slug.to_string(),
            title: format!("Finding: {}", slug),
            impact: impact.to_string(),
            quality_score: 4.0,
            firm: "TestFirm".to_string(),
            protocol: "TestProtocol".to_string(),
            tags: vec!["Reentrancy".to_string()],
            category: "Lending".to_string(),
            summary: Some("Test summary".to_string()),
            content: None,
        }
    }

    #[test]
    fn ingest_writes_to_raw_cache() {
        let (_tmp, store) = temp_store();
        let raw = RawCache::new(&store);
        let count = raw.ingest(&sample_params(), vec![sample_finding("test-1", "HIGH")]).unwrap();
        assert_eq!(count, 1);
        assert!(raw.is_cached(&sample_params()).unwrap());
    }

    #[test]
    fn ingest_skips_when_cache_fresh() {
        let (_tmp, store) = temp_store();
        let raw = RawCache::new(&store);
        let params = sample_params();
        assert_eq!(raw.ingest(&params, vec![sample_finding("a", "HIGH")]).unwrap(), 1);
        assert_eq!(raw.ingest(&params, vec![sample_finding("b", "HIGH")]).unwrap(), 0);
    }

    #[test]
    fn ingest_filters_invalid_impact() {
        let (_tmp, store) = temp_store();
        let raw = RawCache::new(&store);
        let findings = vec![
            sample_finding("high-one", "HIGH"),
            sample_finding("low-one", "LOW"),
            sample_finding("med-one", "MEDIUM"),
        ];
        let count = raw.ingest(&sample_params(), findings).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn evict_expired_removes_stale_files() {
        let (_tmp, store) = temp_store();
        let raw = RawCache::new(&store);
        let fresh = RawQueryParams { keywords: "keep".to_string(), ..sample_params() };
        let stale = RawQueryParams { keywords: "remove".to_string(), ..sample_params() };
        raw.ingest(&fresh, vec![sample_finding("keep-1", "HIGH")]).unwrap();
        raw.ingest_with_ttl(&stale, vec![sample_finding("rm-1", "HIGH")], 0).unwrap();
        assert_eq!(raw.evict_expired().unwrap(), 1);
        assert_eq!(raw.all_entries().unwrap().len(), 1);
    }
}
