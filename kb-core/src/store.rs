use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::types::{CuratedFile, Impact, KbEntry, RawCacheEnvelope, SeedFile};

/// Filesystem operations for the KnowledgeBase directory.
///
/// All JSON I/O is isolated here so the core logic is testable with temp dirs.
pub struct Store {
    base_dir: PathBuf,
}

/// Subdirectories under seeds/ for different abstraction levels.
const SEED_SUBDIRS: &[&str] = &["code", "design", "shared"];

impl Store {
    /// Create a store rooted at the given directory.
    /// Creates `raw/`, `curated/`, and `seeds/{code,design,shared}/` subdirectories if missing.
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(base_dir.join("raw"))
            .with_context(|| format!("creating raw/ in {}", base_dir.display()))?;
        fs::create_dir_all(base_dir.join("curated"))
            .with_context(|| format!("creating curated/ in {}", base_dir.display()))?;
        for subdir in SEED_SUBDIRS {
            fs::create_dir_all(base_dir.join("seeds").join(subdir))
                .with_context(|| format!("creating seeds/{}/ in {}", subdir, base_dir.display()))?;
        }
        Ok(Self { base_dir })
    }

    // -- Raw cache --

    fn raw_path(&self, fingerprint: &str) -> PathBuf {
        self.base_dir.join("raw").join(format!("{}.json", fingerprint))
    }

    /// Read a raw cache envelope by fingerprint. Returns None if not found.
    pub fn read_raw(&self, fingerprint: &str) -> Result<Option<RawCacheEnvelope>> {
        let path = self.raw_path(fingerprint);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let envelope: RawCacheEnvelope = serde_json::from_str(&data)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(envelope))
    }

    /// Write a raw cache envelope to disk.
    pub fn write_raw(&self, envelope: &RawCacheEnvelope) -> Result<()> {
        let path = self.raw_path(&envelope.fingerprint);
        let json = serde_json::to_string_pretty(envelope)
            .context("serializing raw cache envelope")?;
        fs::write(&path, json)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Delete a raw cache file by fingerprint.
    pub fn delete_raw(&self, fingerprint: &str) -> Result<()> {
        let path = self.raw_path(fingerprint);
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("deleting {}", path.display()))?;
        }
        Ok(())
    }

    /// List all raw cache envelopes.
    pub fn list_raw(&self) -> Result<Vec<RawCacheEnvelope>> {
        let raw_dir = self.base_dir.join("raw");
        let mut envelopes = Vec::new();
        for entry in fs::read_dir(&raw_dir).with_context(|| format!("listing {}", raw_dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let data = fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                match serde_json::from_str::<RawCacheEnvelope>(&data) {
                    Ok(env) => envelopes.push(env),
                    Err(e) => {
                        tracing::warn!("skipping malformed raw cache file {}: {}", path.display(), e);
                    }
                }
            }
        }
        Ok(envelopes)
    }

    // -- Curated --

    fn curated_path(&self, impact: &Impact) -> PathBuf {
        let name = match impact {
            Impact::High => "high.json",
            Impact::Medium => "medium.json",
        };
        self.base_dir.join("curated").join(name)
    }

    /// Read a curated file for a given impact level. Returns None if not found.
    pub fn read_curated(&self, impact: &Impact) -> Result<Option<CuratedFile>> {
        let path = self.curated_path(impact);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: CuratedFile = serde_json::from_str(&data)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(file))
    }

    /// Write a curated file for a given impact level.
    pub fn write_curated(&self, file: &CuratedFile) -> Result<()> {
        let path = self.curated_path(&file.impact);
        let json = serde_json::to_string_pretty(file)
            .context("serializing curated file")?;
        fs::write(&path, json)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    // -- Seeds --

    /// Read all seed files from `seeds/` and its subdirectories (code/, design/, shared/).
    pub fn read_seeds(&self) -> Result<Vec<SeedFile>> {
        let seeds_dir = self.base_dir.join("seeds");
        let mut seed_files = Vec::new();

        // Read from each subdirectory
        for subdir in SEED_SUBDIRS {
            let subdir_path = seeds_dir.join(subdir);
            if subdir_path.exists() {
                self.read_seeds_from_dir(&subdir_path, &mut seed_files)?;
            }
        }

        // Also read any JSON files directly in seeds/ (backward compat)
        self.read_seeds_from_dir(&seeds_dir, &mut seed_files)?;

        Ok(seed_files)
    }

    /// Read seed files from a single directory (non-recursive).
    fn read_seeds_from_dir(&self, dir: &Path, out: &mut Vec<SeedFile>) -> Result<()> {
        for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                let data = fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                match serde_json::from_str::<SeedFile>(&data) {
                    Ok(sf) => out.push(sf),
                    Err(e) => {
                        tracing::warn!("skipping malformed seed file {}: {}", path.display(), e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Write a seed file. If the seed has a level, writes to the appropriate subdir.
    pub fn write_seed(&self, seed: &SeedFile) -> Result<()> {
        let filename = seed.domain.to_lowercase().replace(' ', "-");
        let subdir = match seed.level {
            crate::types::SeedLevel::Code => "code",
            crate::types::SeedLevel::Design => "design",
            crate::types::SeedLevel::Both => "shared",
        };
        let path = self.base_dir
            .join("seeds")
            .join(subdir)
            .join(format!("{}.json", filename));
        let json = serde_json::to_string_pretty(seed)
            .context("serializing seed file")?;
        fs::write(&path, json)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// List seed file paths across all subdirectories.
    pub fn list_seed_paths(&self) -> Result<Vec<PathBuf>> {
        let seeds_dir = self.base_dir.join("seeds");
        let mut paths = Vec::new();

        for subdir in SEED_SUBDIRS {
            let subdir_path = seeds_dir.join(subdir);
            if subdir_path.exists() {
                self.list_json_in_dir(&subdir_path, &mut paths)?;
            }
        }

        // Also list root seeds/ (backward compat)
        self.list_json_in_dir(&seeds_dir, &mut paths)?;

        Ok(paths)
    }

    fn list_json_in_dir(&self, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                out.push(path);
            }
        }
        Ok(())
    }

    // -- Helpers --

    /// Find a KbEntry by ID across all curated files.
    pub fn find_entry_mut(&self, entry_id: &str) -> Result<Option<(Impact, KbEntry)>> {
        for impact in &[Impact::High, Impact::Medium] {
            if let Some(file) = self.read_curated(impact)? {
                if let Some(entry) = file.entries.iter().find(|e| e.id == entry_id) {
                    return Ok(Some((impact.clone(), entry.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Update a single entry in the curated files by ID.
    /// Calls the provided closure to mutate the entry, then writes back.
    pub fn update_curated_entry<F>(&self, entry_id: &str, updater: F) -> Result<bool>
    where
        F: FnOnce(&mut KbEntry),
    {
        for impact in &[Impact::High, Impact::Medium] {
            if let Some(mut file) = self.read_curated(impact)? {
                if let Some(entry) = file.entries.iter_mut().find(|e| e.id == entry_id) {
                    updater(entry);
                    self.write_curated(&file)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Base directory path.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;
    use std::fs;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().to_path_buf()).unwrap();
        (tmp, store)
    }

    fn sample_entry(id: &str, impact: Impact) -> KbEntry {
        KbEntry {
            id: id.to_string(),
            slug: id.to_string(),
            title: format!("Finding: {}", id),
            impact,
            quality_score: 4.0,
            firm: "TestFirm".to_string(),
            protocol: "TestProtocol".to_string(),
            tags: vec!["Reentrancy".to_string()],
            category: "Lending".to_string(),
            summary: Some("Test summary".to_string()),
            content: None,
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
        }
    }

    fn sample_envelope(fingerprint: &str) -> RawCacheEnvelope {
        RawCacheEnvelope {
            fingerprint: fingerprint.to_string(),
            query_params: RawQueryParams {
                keywords: "reentrancy".to_string(),
                impact: vec!["HIGH".to_string()],
                tags: vec![],
                protocol_categories: vec![],
                min_quality: None,
            },
            fetched_at: Utc::now(),
            ttl_secs: 300,
            entries: vec![sample_entry("solodit:test-1", Impact::High)],
        }
    }

    #[test]
    fn raw_roundtrip() {
        let (_tmp, store) = temp_store();
        let env = sample_envelope("abc123");
        store.write_raw(&env).unwrap();
        let loaded = store.read_raw("abc123").unwrap().unwrap();
        assert_eq!(loaded.fingerprint, "abc123");
        assert_eq!(loaded.entries.len(), 1);
    }

    #[test]
    fn raw_missing_returns_none() {
        let (_tmp, store) = temp_store();
        assert!(store.read_raw("nonexistent").unwrap().is_none());
    }

    #[test]
    fn raw_delete() {
        let (_tmp, store) = temp_store();
        store.write_raw(&sample_envelope("to-delete")).unwrap();
        store.delete_raw("to-delete").unwrap();
        assert!(store.read_raw("to-delete").unwrap().is_none());
    }

    #[test]
    fn raw_list() {
        let (_tmp, store) = temp_store();
        store.write_raw(&sample_envelope("fp1")).unwrap();
        store.write_raw(&sample_envelope("fp2")).unwrap();
        assert_eq!(store.list_raw().unwrap().len(), 2);
    }

    #[test]
    fn raw_skips_malformed_files() {
        let (_tmp, store) = temp_store();
        store.write_raw(&sample_envelope("valid")).unwrap();
        let bad_path = store.base_dir().join("raw").join("bad.json");
        fs::write(&bad_path, "not valid json").unwrap();
        assert_eq!(store.list_raw().unwrap().len(), 1);
    }

    #[test]
    fn curated_roundtrip() {
        let (_tmp, store) = temp_store();
        let file = CuratedFile {
            impact: Impact::High,
            last_curated_at: Utc::now(),
            entries: vec![sample_entry("solodit:high-1", Impact::High)],
        };
        store.write_curated(&file).unwrap();
        let loaded = store.read_curated(&Impact::High).unwrap().unwrap();
        assert_eq!(loaded.entries.len(), 1);
    }

    #[test]
    fn curated_missing_returns_none() {
        let (_tmp, store) = temp_store();
        assert!(store.read_curated(&Impact::High).unwrap().is_none());
    }

    #[test]
    fn update_curated_entry_by_id() {
        let (_tmp, store) = temp_store();
        let file = CuratedFile {
            impact: Impact::High,
            last_curated_at: Utc::now(),
            entries: vec![sample_entry("solodit:target", Impact::High)],
        };
        store.write_curated(&file).unwrap();
        let updated = store
            .update_curated_entry("solodit:target", |e| {
                e.curation = CurationStatus::Useful;
            })
            .unwrap();
        assert!(updated);
        let loaded = store.read_curated(&Impact::High).unwrap().unwrap();
        assert_eq!(loaded.entries[0].curation, CurationStatus::Useful);
    }

    #[test]
    fn seed_roundtrip_in_subdirs() {
        let (_tmp, store) = temp_store();
        let seed = SeedFile {
            domain: "ERC4626".to_string(),
            description: "Known vault bugs".to_string(),
            level: SeedLevel::Code,
            entries: vec![sample_entry("seed:inflation-attack", Impact::High)],
        };
        store.write_seed(&seed).unwrap();
        let loaded = store.read_seeds().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].domain, "ERC4626");
    }

    #[test]
    fn seed_writes_to_correct_subdir() {
        let (_tmp, store) = temp_store();

        let code_seed = SeedFile {
            domain: "code-test".to_string(),
            description: "test".to_string(),
            level: SeedLevel::Code,
            entries: vec![],
        };
        let design_seed = SeedFile {
            domain: "design-test".to_string(),
            description: "test".to_string(),
            level: SeedLevel::Design,
            entries: vec![],
        };
        let shared_seed = SeedFile {
            domain: "shared-test".to_string(),
            description: "test".to_string(),
            level: SeedLevel::Both,
            entries: vec![],
        };

        store.write_seed(&code_seed).unwrap();
        store.write_seed(&design_seed).unwrap();
        store.write_seed(&shared_seed).unwrap();

        let paths = store.list_seed_paths().unwrap();
        assert_eq!(paths.len(), 3);

        let loaded = store.read_seeds().unwrap();
        assert_eq!(loaded.len(), 3);
    }
}
