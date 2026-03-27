use anyhow::Result;

use crate::store::Store;
use crate::types::{CurationStatus, Impact, KbEntry, KbQuery, KbQueryResult, Pipeline};

/// Layer 3 — Agent consumption: filtered, ranked, budget-truncated queries.
///
/// Reads curated files in severity order (HIGH → MEDIUM), applies filters,
/// applies pipeline-aware ranking boost, and truncates to the caller's context budget.
pub struct QueryEngine<'a> {
    store: &'a Store,
}

impl<'a> QueryEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Query the curated index for agent consumption.
    pub fn query(&self, q: &KbQuery) -> Result<KbQueryResult> {
        let mut all_entries = Vec::new();

        // Read in severity order: HIGH first, then MEDIUM
        for impact in &[Impact::High, Impact::Medium] {
            if let Some(file) = self.store.read_curated(impact)? {
                all_entries.extend(file.entries);
            }
        }

        // Apply filters
        let mut filtered: Vec<KbEntry> = all_entries
            .into_iter()
            .filter(|e| {
                if q.exclude_noise && e.curation == CurationStatus::Noise {
                    return false;
                }
                if !q.tags.is_empty() && !has_overlap(&e.tags, &q.tags) {
                    return false;
                }
                if !q.categories.is_empty() && !matches_category(&e.category, &q.categories) {
                    return false;
                }
                if !q.keywords.is_empty() && !matches_keywords(e, &q.keywords) {
                    return false;
                }
                true
            })
            .collect();

        // Apply pipeline-aware re-ranking if pipeline is specified.
        // Entries with context at the requested abstraction level get a boost.
        // This is a stable sort within each severity tier — entries without
        // context are still returned, just ranked lower.
        if let Some(ref pipeline) = q.pipeline {
            stable_pipeline_boost(&mut filtered, pipeline);
        }

        let total_available = filtered.len();
        let truncated = total_available > q.max_entries;

        let entries = filtered.into_iter().take(q.max_entries).collect();

        Ok(KbQueryResult {
            entries,
            total_available,
            truncated,
        })
    }
}

/// Re-rank entries based on pipeline context availability.
/// Entries with context at the requested level sort before entries without,
/// within the same severity tier and curation rank.
fn stable_pipeline_boost(entries: &mut [KbEntry], pipeline: &Pipeline) {
    entries.sort_by(|a, b| {
        // Primary: severity (HIGH before MEDIUM)
        let sev_cmp = b.impact.rank().cmp(&a.impact.rank());
        if sev_cmp != std::cmp::Ordering::Equal {
            return sev_cmp;
        }

        // Secondary: curation rank
        let cur_cmp = b.curation.rank().cmp(&a.curation.rank());
        if cur_cmp != std::cmp::Ordering::Equal {
            return cur_cmp;
        }

        // Tertiary: pipeline context availability (has context > no context)
        let a_has = has_pipeline_context(a, pipeline);
        let b_has = has_pipeline_context(b, pipeline);
        b_has.cmp(&a_has)
            .then(
                b.relevance_score
                    .partial_cmp(&a.relevance_score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
}

fn has_pipeline_context(entry: &KbEntry, pipeline: &Pipeline) -> bool {
    match pipeline {
        Pipeline::DesignReview => entry.design_context.is_some(),
        Pipeline::AuditSandbox => entry.code_context.is_some(),
    }
}

/// Check if any entry tag matches any query tag (case-insensitive).
fn has_overlap(entry_tags: &[String], query_tags: &[String]) -> bool {
    entry_tags.iter().any(|et| {
        query_tags
            .iter()
            .any(|qt| et.eq_ignore_ascii_case(qt))
    })
}

/// Check if entry category matches any query category (case-insensitive).
fn matches_category(entry_cat: &str, query_cats: &[String]) -> bool {
    query_cats
        .iter()
        .any(|qc| entry_cat.eq_ignore_ascii_case(qc))
}

/// Check if any keyword appears in the entry's title or summary (case-insensitive).
fn matches_keywords(entry: &KbEntry, keywords: &[String]) -> bool {
    let title_lower = entry.title.to_lowercase();
    let summary_lower = entry
        .summary
        .as_deref()
        .unwrap_or("")
        .to_lowercase();

    keywords.iter().any(|kw| {
        let kw_lower = kw.to_lowercase();
        title_lower.contains(&kw_lower) || summary_lower.contains(&kw_lower)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().to_path_buf()).unwrap();
        (tmp, store)
    }

    fn make_entry(
        id: &str,
        impact: Impact,
        quality: f64,
        curation: CurationStatus,
        tags: &[&str],
        category: &str,
    ) -> KbEntry {
        KbEntry {
            id: id.to_string(),
            slug: id.to_string(),
            title: format!("Finding: {}", id),
            impact,
            quality_score: quality,
            firm: "Firm".to_string(),
            protocol: "Proto".to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            category: category.to_string(),
            summary: Some(format!("Summary for {}", id)),
            content: None,
            design_context: None,
            code_context: None,
            source: EntrySource::Solodit,
            curation,
            relevance_score: 0.5,
            confidence: 0.5,
            ingested_at: Utc::now(),
            last_curated_at: None,
            auditor_notes: None,
            confirmed_by: vec![],
        }
    }

    fn seed_curated(store: &Store, high: Vec<KbEntry>, medium: Vec<KbEntry>) {
        let now = Utc::now();
        store
            .write_curated(&CuratedFile {
                impact: Impact::High,
                last_curated_at: now,
                entries: high,
            })
            .unwrap();
        store
            .write_curated(&CuratedFile {
                impact: Impact::Medium,
                last_curated_at: now,
                entries: medium,
            })
            .unwrap();
    }

    #[test]
    fn query_returns_all_when_no_filters() {
        let (_tmp, store) = temp_store();
        seed_curated(
            &store,
            vec![make_entry("h1", Impact::High, 4.0, CurationStatus::Unreviewed, &["Reentrancy"], "Lending")],
            vec![make_entry("m1", Impact::Medium, 3.0, CurationStatus::Unreviewed, &["ERC4626"], "Yield")],
        );
        let engine = QueryEngine::new(&store);
        let result = engine.query(&KbQuery { max_entries: 100, ..Default::default() }).unwrap();
        assert_eq!(result.entries.len(), 2);
        assert!(!result.truncated);
    }

    #[test]
    fn query_severity_order_high_before_medium() {
        let (_tmp, store) = temp_store();
        seed_curated(
            &store,
            vec![make_entry("h1", Impact::High, 4.0, CurationStatus::Unreviewed, &[], "")],
            vec![make_entry("m1", Impact::Medium, 5.0, CurationStatus::Critical, &[], "")],
        );
        let engine = QueryEngine::new(&store);
        let result = engine.query(&KbQuery { max_entries: 100, ..Default::default() }).unwrap();
        assert_eq!(result.entries[0].id, "h1");
    }

    #[test]
    fn query_filter_by_tags() {
        let (_tmp, store) = temp_store();
        seed_curated(
            &store,
            vec![
                make_entry("reent", Impact::High, 4.0, CurationStatus::Unreviewed, &["Reentrancy"], "Lending"),
                make_entry("oracle", Impact::High, 4.0, CurationStatus::Unreviewed, &["Oracle"], "Lending"),
            ],
            vec![],
        );
        let engine = QueryEngine::new(&store);
        let result = engine
            .query(&KbQuery {
                tags: vec!["Reentrancy".to_string()],
                max_entries: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].id, "reent");
    }

    #[test]
    fn query_excludes_noise_by_default() {
        let (_tmp, store) = temp_store();
        seed_curated(
            &store,
            vec![
                make_entry("good", Impact::High, 4.0, CurationStatus::Useful, &[], ""),
                make_entry("bad", Impact::High, 4.0, CurationStatus::Noise, &[], ""),
            ],
            vec![],
        );
        let engine = QueryEngine::new(&store);
        let result = engine.query(&KbQuery { max_entries: 100, ..Default::default() }).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].id, "good");
    }

    #[test]
    fn query_truncates_to_budget() {
        let (_tmp, store) = temp_store();
        seed_curated(
            &store,
            vec![
                make_entry("h1", Impact::High, 5.0, CurationStatus::Critical, &[], ""),
                make_entry("h2", Impact::High, 4.0, CurationStatus::Useful, &[], ""),
                make_entry("h3", Impact::High, 3.0, CurationStatus::Unreviewed, &[], ""),
            ],
            vec![make_entry("m1", Impact::Medium, 3.0, CurationStatus::Unreviewed, &[], "")],
        );
        let engine = QueryEngine::new(&store);
        let result = engine.query(&KbQuery { max_entries: 2, ..Default::default() }).unwrap();
        assert_eq!(result.entries.len(), 2);
        assert!(result.truncated);
        assert_eq!(result.total_available, 4);
    }

    #[test]
    fn query_pipeline_boost_ranks_context_entries_higher() {
        let (_tmp, store) = temp_store();

        let mut with_design = make_entry("design-aware", Impact::High, 4.0, CurationStatus::Unreviewed, &["ERC4626"], "");
        with_design.design_context = Some(AbstractionContext {
            indicators: vec!["missing minimum deposit".to_string()],
            description: None,
        });

        let without_design = make_entry("code-only", Impact::High, 4.0, CurationStatus::Unreviewed, &["ERC4626"], "");

        // Seed with code-only first to ensure the boost re-orders
        seed_curated(&store, vec![without_design, with_design], vec![]);

        let engine = QueryEngine::new(&store);
        let result = engine
            .query(&KbQuery {
                pipeline: Some(Pipeline::DesignReview),
                max_entries: 100,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].id, "design-aware");
        assert_eq!(result.entries[1].id, "code-only");
    }

    #[test]
    fn query_empty_curated_returns_empty() {
        let (_tmp, store) = temp_store();
        let engine = QueryEngine::new(&store);
        let result = engine.query(&KbQuery::default()).unwrap();
        assert_eq!(result.entries.len(), 0);
    }
}
