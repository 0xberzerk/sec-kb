use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;

use crate::store::Store;
use crate::types::{
    CuratedFile, CurationContext, CurationStats, CurationStatus, FeedbackItem, Impact, KbEntry,
    PipelineConfirmation,
};

/// Layer 2 — Curation: scoring, deduplication, severity bucketing.
pub struct Curator<'a> {
    store: &'a Store,
}

impl<'a> Curator<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Run a curation pass over all provided entries.
    pub fn curate(
        &self,
        entries: Vec<KbEntry>,
        context: &CurationContext,
    ) -> Result<CurationStats> {
        let existing = self.load_existing_curation()?;
        let deduped = deduplicate(entries);

        let mut high_entries = Vec::new();
        let mut medium_entries = Vec::new();
        let mut noise_skipped = 0;

        for mut entry in deduped {
            // Restore existing curation status
            if let Some(prev) = existing.get(&entry.id) {
                entry.curation = prev.curation.clone();
                entry.auditor_notes = prev.auditor_notes.clone();
                if prev.last_curated_at.is_some() {
                    entry.last_curated_at = prev.last_curated_at;
                }
                // Preserve confirmed_by from previous curation
                if !prev.confirmed_by.is_empty() {
                    entry.confirmed_by = prev.confirmed_by.clone();
                }
            }

            if entry.curation == CurationStatus::Noise {
                noise_skipped += 1;
                continue;
            }

            score_entry(&mut entry, context);

            match entry.impact {
                Impact::High => high_entries.push(entry),
                Impact::Medium => medium_entries.push(entry),
            }
        }

        sort_entries(&mut high_entries);
        sort_entries(&mut medium_entries);

        let stats = CurationStats {
            total_processed: high_entries.len() + medium_entries.len() + noise_skipped,
            high_count: high_entries.len(),
            medium_count: medium_entries.len(),
            noise_skipped,
        };

        let now = Utc::now();

        self.store.write_curated(&CuratedFile {
            impact: Impact::High,
            last_curated_at: now,
            entries: high_entries,
        })?;

        self.store.write_curated(&CuratedFile {
            impact: Impact::Medium,
            last_curated_at: now,
            entries: medium_entries,
        })?;

        Ok(stats)
    }

    /// Update curation status for a single entry by ID.
    pub fn set_curation(
        &self,
        entry_id: &str,
        status: CurationStatus,
        notes: Option<String>,
    ) -> Result<bool> {
        let now = Utc::now();
        self.store.update_curated_entry(entry_id, |entry| {
            entry.curation = status;
            entry.last_curated_at = Some(now);
            if let Some(n) = notes {
                entry.auditor_notes = Some(n);
            }
            entry.confidence = confidence_from_curation(&entry.curation, entry.relevance_score);
        })
    }

    /// Apply bulk feedback from auditor review.
    /// When pipeline is provided, also records a PipelineConfirmation.
    pub fn apply_feedback(&self, feedback: &[FeedbackItem]) -> Result<usize> {
        let mut updated = 0;
        for item in feedback {
            let notes = item.reason.clone();
            let pipeline = item.pipeline.clone();

            // First update the curation status
            if self.set_curation(&item.entry_id, item.new_status.clone(), notes)? {
                // Then record pipeline confirmation if pipeline is provided
                // and the status is Useful or Critical (positive confirmation)
                if let Some(p) = pipeline {
                    if item.new_status == CurationStatus::Useful
                        || item.new_status == CurationStatus::Critical
                    {
                        let now = Utc::now();
                        let confirmation = PipelineConfirmation {
                            pipeline: p,
                            confirmed_at: now,
                            context: item.reason.clone(),
                        };
                        self.store.update_curated_entry(&item.entry_id, |entry| {
                            // Avoid duplicate confirmations from same pipeline
                            if !entry.confirmed_by.iter().any(|c| c.pipeline == confirmation.pipeline) {
                                entry.confirmed_by.push(confirmation.clone());
                            }
                        })?;
                    }
                }
                updated += 1;
            }
        }
        Ok(updated)
    }

    fn load_existing_curation(&self) -> Result<HashMap<String, CurationSnapshot>> {
        let mut map = HashMap::new();
        for impact in &[Impact::High, Impact::Medium] {
            if let Some(file) = self.store.read_curated(impact)? {
                for entry in file.entries {
                    map.insert(
                        entry.id.clone(),
                        CurationSnapshot {
                            curation: entry.curation,
                            auditor_notes: entry.auditor_notes,
                            last_curated_at: entry.last_curated_at,
                            confirmed_by: entry.confirmed_by,
                        },
                    );
                }
            }
        }
        Ok(map)
    }
}

struct CurationSnapshot {
    curation: CurationStatus,
    auditor_notes: Option<String>,
    last_curated_at: Option<chrono::DateTime<Utc>>,
    confirmed_by: Vec<PipelineConfirmation>,
}

fn deduplicate(entries: Vec<KbEntry>) -> Vec<KbEntry> {
    let mut by_slug: HashMap<String, KbEntry> = HashMap::new();
    for entry in entries {
        by_slug
            .entry(entry.slug.clone())
            .and_modify(|existing| {
                if entry.quality_score > existing.quality_score {
                    *existing = entry.clone();
                }
            })
            .or_insert(entry);
    }
    by_slug.into_values().collect()
}

fn score_entry(entry: &mut KbEntry, context: &CurationContext) {
    let quality_norm = (entry.quality_score / 5.0).clamp(0.0, 1.0);
    let impact_norm = entry.impact.rank() as f64 / 2.0;
    let base = quality_norm * 0.5 + impact_norm * 0.3;

    let context_boost = if context_is_empty(context) {
        0.1
    } else {
        let tag_boost = tag_overlap_score(&entry.tags, &context.integration_types);
        let category_boost = category_match_score(&entry.category, &context.protocol_categories);
        let keyword_boost = keyword_overlap_score(entry, &context.codebase_keywords);
        (tag_boost + category_boost + keyword_boost).clamp(0.0, 0.2)
    };

    entry.relevance_score = (base + context_boost).clamp(0.0, 1.0);
    entry.confidence = confidence_from_curation(&entry.curation, entry.relevance_score);
    entry.last_curated_at = Some(Utc::now());
}

fn context_is_empty(ctx: &CurationContext) -> bool {
    ctx.codebase_keywords.is_empty()
        && ctx.integration_types.is_empty()
        && ctx.protocol_categories.is_empty()
}

fn tag_overlap_score(entry_tags: &[String], integration_types: &[String]) -> f64 {
    if integration_types.is_empty() || entry_tags.is_empty() {
        return 0.0;
    }
    let matches = entry_tags
        .iter()
        .filter(|t| {
            let t_lower = t.to_lowercase();
            integration_types
                .iter()
                .any(|i| i.to_lowercase().contains(&t_lower) || t_lower.contains(&i.to_lowercase()))
        })
        .count();
    let ratio = matches as f64 / entry_tags.len().max(1) as f64;
    ratio * 0.067
}

fn category_match_score(entry_category: &str, protocol_categories: &[String]) -> f64 {
    if protocol_categories.is_empty() {
        return 0.0;
    }
    let cat_lower = entry_category.to_lowercase();
    if protocol_categories
        .iter()
        .any(|c| c.to_lowercase() == cat_lower)
    {
        0.067
    } else {
        0.0
    }
}

fn keyword_overlap_score(entry: &KbEntry, keywords: &[String]) -> f64 {
    if keywords.is_empty() {
        return 0.0;
    }
    let searchable = format!(
        "{} {}",
        entry.title.to_lowercase(),
        entry.summary.as_deref().unwrap_or("").to_lowercase()
    );
    let matches = keywords
        .iter()
        .filter(|kw| searchable.contains(&kw.to_lowercase()))
        .count();
    let ratio = matches as f64 / keywords.len().max(1) as f64;
    ratio * 0.067
}

fn confidence_from_curation(curation: &CurationStatus, relevance: f64) -> f64 {
    match curation {
        CurationStatus::Critical => 0.95,
        CurationStatus::Useful => 0.80,
        CurationStatus::Unreviewed => relevance * 0.7,
        CurationStatus::Noise => 0.0,
    }
}

fn sort_entries(entries: &mut [KbEntry]) {
    entries.sort_by(|a, b| {
        b.curation
            .rank()
            .cmp(&a.curation.rank())
            .then(
                b.relevance_score
                    .partial_cmp(&a.relevance_score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                b.quality_score
                    .partial_cmp(&a.quality_score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
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

    fn make_entry(id: &str, impact: Impact, quality: f64, curation: CurationStatus) -> KbEntry {
        KbEntry {
            id: id.to_string(),
            slug: id.to_string(),
            title: format!("Finding: {}", id),
            impact,
            quality_score: quality,
            firm: "Firm".to_string(),
            protocol: "Proto".to_string(),
            tags: vec!["Reentrancy".to_string()],
            category: "Lending".to_string(),
            summary: Some("summary".to_string()),
            content: None,
            design_context: None,
            code_context: None,
            source: EntrySource::Solodit,
            curation,
            relevance_score: 0.0,
            confidence: 0.0,
            ingested_at: Utc::now(),
            last_curated_at: None,
            auditor_notes: None,
            confirmed_by: vec![],
            contributor: None,
        }
    }

    fn empty_context() -> CurationContext {
        CurationContext::default()
    }

    #[test]
    fn curate_buckets_by_impact() {
        let (_tmp, store) = temp_store();
        let curator = Curator::new(&store);
        let entries = vec![
            make_entry("h1", Impact::High, 4.0, CurationStatus::Unreviewed),
            make_entry("m1", Impact::Medium, 3.0, CurationStatus::Unreviewed),
            make_entry("h2", Impact::High, 5.0, CurationStatus::Unreviewed),
        ];
        let stats = curator.curate(entries, &empty_context()).unwrap();
        assert_eq!(stats.high_count, 2);
        assert_eq!(stats.medium_count, 1);
    }

    #[test]
    fn curate_excludes_noise() {
        let (_tmp, store) = temp_store();
        let curator = Curator::new(&store);
        let entries = vec![
            make_entry("good", Impact::High, 4.0, CurationStatus::Unreviewed),
            make_entry("noisy", Impact::High, 3.0, CurationStatus::Noise),
        ];
        let stats = curator.curate(entries, &empty_context()).unwrap();
        assert_eq!(stats.high_count, 1);
        assert_eq!(stats.noise_skipped, 1);
    }

    #[test]
    fn curate_preserves_existing_curation() {
        let (_tmp, store) = temp_store();
        let curator = Curator::new(&store);

        let entries = vec![make_entry("a", Impact::High, 4.0, CurationStatus::Unreviewed)];
        curator.curate(entries, &empty_context()).unwrap();
        curator.set_curation("a", CurationStatus::Useful, Some("good one".to_string())).unwrap();

        let entries = vec![make_entry("a", Impact::High, 4.0, CurationStatus::Unreviewed)];
        curator.curate(entries, &empty_context()).unwrap();

        let high = store.read_curated(&Impact::High).unwrap().unwrap();
        assert_eq!(high.entries[0].curation, CurationStatus::Useful);
        assert_eq!(high.entries[0].auditor_notes.as_deref(), Some("good one"));
    }

    #[test]
    fn apply_feedback_records_pipeline_confirmation() {
        let (_tmp, store) = temp_store();
        let curator = Curator::new(&store);

        let entries = vec![make_entry("confirmed", Impact::High, 4.0, CurationStatus::Unreviewed)];
        curator.curate(entries, &empty_context()).unwrap();

        let feedback = vec![FeedbackItem {
            entry_id: "confirmed".to_string(),
            new_status: CurationStatus::Useful,
            reason: Some("valid finding".to_string()),
            pipeline: Some(Pipeline::DesignReview),
        }];
        let updated = curator.apply_feedback(&feedback).unwrap();
        assert_eq!(updated, 1);

        let high = store.read_curated(&Impact::High).unwrap().unwrap();
        let entry = &high.entries[0];
        assert_eq!(entry.curation, CurationStatus::Useful);
        assert_eq!(entry.confirmed_by.len(), 1);
        assert_eq!(entry.confirmed_by[0].pipeline, Pipeline::DesignReview);
    }

    #[test]
    fn confidence_from_curation_values() {
        assert_eq!(confidence_from_curation(&CurationStatus::Critical, 0.5), 0.95);
        assert_eq!(confidence_from_curation(&CurationStatus::Useful, 0.5), 0.80);
        assert!((confidence_from_curation(&CurationStatus::Unreviewed, 0.8) - 0.56).abs() < 0.001);
        assert_eq!(confidence_from_curation(&CurationStatus::Noise, 0.9), 0.0);
    }
}
