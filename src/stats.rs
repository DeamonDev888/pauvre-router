//! Statistiques d'usage + persistance de l'état de santé.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct Stats {
    pub requests_total: u64,
    pub ok_total: u64,
    pub fallback_total: u64,
    pub cache_hits: u64,
    pub spam_blocked: u64,
    pub busy_rejected: u64,
    pub errors_total: u64,
    /// par modèle : ok / erreurs
    pub per_model: std::collections::HashMap<String, ModelStat>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct ModelStat {
    pub ok: u64,
    pub errors: u64,
    pub exhausted: u64,
}

impl Stats {
    pub fn note_ok(&mut self, model: &str) {
        self.requests_total += 1;
        self.ok_total += 1;
        self.per_model.entry(model.to_string()).or_default().ok += 1;
    }
    pub fn note_fallback(&mut self, model: &str) {
        self.requests_total += 1;
        self.fallback_total += 1;
        self.per_model.entry(model.to_string()).or_default().ok += 1;
    }
    pub fn note_error(&mut self, model: &str, exhausted: bool) {
        self.requests_total += 1;
        self.errors_total += 1;
        let s = self.per_model.entry(model.to_string()).or_default();
        s.errors += 1;
        if exhausted {
            s.exhausted += 1;
        }
    }
}

/// Fichier d'état (à côté de l'exe ou override env PAUVRE_STATE).
pub fn state_path() -> PathBuf {
    std::env::var("PAUVRE_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                .join(".pauvre-router-state.json")
        })
}

/// Sauvegarde atomique (tmp + rename).
pub fn save(stats: &Stats, registry_dump: &serde_json::Value) -> std::io::Result<()> {
    let p = state_path();
    let doc = serde_json::json!({
        "saved_at_epoch": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "stats": stats,
        "health": registry_dump,
    });
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap())?;
    std::fs::rename(&tmp, &p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_comptent() {
        let mut s = Stats::default();
        s.note_ok("m1");
        s.note_ok("m1");
        s.note_fallback("m2");
        s.note_error("m3", true);
        s.note_error("m3", false);
        assert_eq!(s.requests_total, 5);
        assert_eq!(s.ok_total, 2);
        assert_eq!(s.fallback_total, 1);
        assert_eq!(s.errors_total, 2);
        assert_eq!(s.per_model["m1"].ok, 2);
        assert_eq!(s.per_model["m3"].exhausted, 1);
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pauvre-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("PAUVRE_STATE", dir.join("state.json"));
        let mut s = Stats::default();
        s.note_ok("space-bunny-free");
        save(&s, &serde_json::json!({})).unwrap();
        let raw = std::fs::read_to_string(dir.join("state.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(doc["stats"]["ok_total"], 1);
        std::env::remove_var("PAUVRE_STATE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
