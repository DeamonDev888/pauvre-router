//! Keystore — pool de clés API Zen avec rotation anti-quota.
//!
//! Mécanique (prouvée en live le 2026-10-16) :
//! - SANS clé (`OPENCODE_API_KEY` unset) : les modèles `-free` passent anonyme.
//! - AVEC une clé (même bidon) : l'anonyme est REFUSÉ (`Invalid credential`,
//!   73 s de latence-piège). Si le pool contient une clé, le spawn part donc
//!   toujours avec une clé — jamais de mix clé/anonyme.
//! - Le pool tourne en round-robin avec santé par clé ; si aucune clé n'est
//!   utilisable, on retombe sur l'anonyme (spawn SANS la var d'env).
//!
//! Format du fichier (JSON) :
//! {
//!   "keys": ["sk-...", "sk-..."],
//!   "disabled": {"sk-...": {"reason": "...", "until_epoch": 0}}
//! }

use crate::classify::ErrorKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Sélection : quelle clé utiliser pour ce spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyChoice {
    /// Clé à passer via OPENCODE_API_KEY.
    Use(String),
    /// Pool vide ou toutes les clés mortes → spawn SANS la var d'env.
    Anonymous,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct KeystoreFile {
    keys: Vec<String>,
    disabled: HashMap<String, DisabledInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DisabledInfo {
    reason: String,
    until_epoch: u64,
}

pub struct KeyStore {
    path: PathBuf,
    keys: Vec<String>,
    disabled: HashMap<String, (String, u64)>,
    /// Round-robin : prochain index à considérer.
    rr: usize,
    /// Mémoire de session : clés connues pour échouer (évite de re-payer
    /// la latence-piège ~70 s par requête).
    session_dead: Vec<String>,
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format accepté : préfixe sk- ou zen-, 20..=200 chars alnum/-/_/..
pub fn is_valid_key_format(k: &str) -> bool {
    k.len() >= 20
        && k.len() <= 200
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && (k.starts_with("sk-") || k.starts_with("zen-"))
}

impl KeyStore {
    /// Ouvre (ou crée vide) le keystore au chemin donné.
    pub fn open(path: PathBuf) -> Self {
        let (keys, disabled) = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<KeystoreFile>(&raw) {
                Ok(f) => (
                    f.keys,
                    f.disabled
                        .into_iter()
                        .map(|(k, v)| (k, (v.reason, v.until_epoch)))
                        .collect(),
                ),
                Err(_) => (Vec::new(), HashMap::new()),
            },
            Err(_) => (Vec::new(), HashMap::new()),
        };
        Self {
            path,
            keys,
            disabled,
            rr: 0,
            session_dead: Vec::new(),
        }
    }

    /// Nombre total de clés connues.
    pub fn count(&self) -> usize {
        self.keys.len()
    }

    /// Clés non désactivées à l'instant `now`.
    pub fn active_count(&self, now: u64) -> usize {
        self.keys
            .iter()
            .filter(|k| !self.is_disabled(k, now))
            .count()
    }

    fn is_disabled(&self, key: &str, now: u64) -> bool {
        match self.disabled.get(key) {
            Some((_reason, until)) => now < *until,
            None => false,
        }
    }

    /// Choisit la clé pour ce spawn : round-robin parmi les actives,
    /// anonyme si pool vide ou tout est mort cette session.
    pub fn select(&mut self) -> KeyChoice {
        if self.keys.is_empty() {
            return KeyChoice::Anonymous;
        }
        let now = now_epoch();
        let n = self.keys.len();
        for i in 0..n {
            let idx = (self.rr + i) % n;
            let k = &self.keys[idx];
            if self.session_dead.iter().any(|d| d == k) {
                continue;
            }
            if self.is_disabled(k, now) {
                continue;
            }
            self.rr = (idx + 1) % n;
            return KeyChoice::Use(k.clone());
        }
        KeyChoice::Anonymous
    }

    /// Marque une clé échouée : morte pour la session + cooldown persistant
    /// (backoff fourni par l'appelant, plafonné à 24 h).
    pub fn note_failure(&mut self, key: &str, kind: ErrorKind, backoff_s: u64) {
        if !self.session_dead.iter().any(|d| d == key) {
            self.session_dead.push(key.to_string());
        }
        let prev = self.disabled.get(key).map(|(_, until)| *until).unwrap_or(0);
        let until = std::cmp::max(now_epoch() + backoff_s.min(86_400), prev);
        let reason = match kind {
            ErrorKind::Exhausted => "quota/limite",
            ErrorKind::ModelInvalid => "modele refuse",
            ErrorKind::Transient => "erreur reseau",
        }
        .to_string();
        self.disabled.insert(key.to_string(), (reason, until));
    }

    /// Marque une clé OK : désactivation effacée, sortie de session_dead.
    pub fn note_success(&mut self, key: &str) {
        self.disabled.remove(key);
        self.session_dead.retain(|k| k != key);
    }

    /// Réactive TOUTES les clés (purge des cooldowns + session_dead).
    pub fn reanimate_all(&mut self) {
        self.disabled.clear();
        self.session_dead.clear();
    }

    /// Ajoute une clé (idempotent, format validé).
    pub fn add(&mut self, key: &str) -> bool {
        if !is_valid_key_format(key) || self.keys.iter().any(|k| k == key) {
            return false;
        }
        self.keys.push(key.to_string());
        self.disabled.remove(key);
        self.session_dead.retain(|k| k != key);
        true
    }

    /// Retire une clé. Retourne true si retirée.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.keys.len();
        self.keys.retain(|k| k != key);
        self.disabled.remove(key);
        self.session_dead.retain(|k| k != key);
        self.keys.len() < before
    }

    /// Écrit le keystore sur disque (atomique tmp + rename).
    pub fn save(&self) -> std::io::Result<()> {
        let f = KeystoreFile {
            keys: self.keys.clone(),
            disabled: self
                .disabled
                .iter()
                .map(|(k, (r, u))| {
                    (
                        k.clone(),
                        DisabledInfo {
                            reason: r.clone(),
                            until_epoch: *u,
                        },
                    )
                })
                .collect(),
        };
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&f).unwrap())?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Méta pour /admin/stats — jamais les valeurs de clés.
    pub fn meta(&self) -> serde_json::Value {
        let now = now_epoch();
        let active = self.active_count(now);
        let disabled = self.keys.len() - active;
        serde_json::json!({
            "total": self.keys.len(),
            "active": active,
            "disabled": disabled,
            "session_dead": self.session_dead.len(),
            "mode": if self.keys.is_empty() { "anonymous" } else { "pool" },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KA: &str = "sk-aaaaaaaaaaaaaaaaaaaa";
    const KB: &str = "sk-bbbbbbbbbbbbbbbbbbbb";

    fn ks() -> KeyStore {
        let dir = std::env::temp_dir().join(format!("pauvre-ks-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut k = KeyStore::open(dir.join(format!("t{}.json", KA.len())));
        k.keys.clear();
        k.disabled.clear();
        k.session_dead.clear();
        k
    }

    #[test]
    fn format_validation() {
        assert!(is_valid_key_format("sk-abc123_-xyz.456789012"));
        assert!(is_valid_key_format("zen-abcdef12345678901234"));
        assert!(!is_valid_key_format("short"));
        assert!(!is_valid_key_format("bad key with spaces 123456"));
        assert!(!is_valid_key_format("no-prefix-123456789012345"));
        assert!(!is_valid_key_format(""));
    }

    #[test]
    fn pool_vide_anonyme() {
        let mut k = ks();
        assert_eq!(k.select(), KeyChoice::Anonymous);
    }

    #[test]
    fn rotation_round_robin() {
        let mut k = ks();
        assert!(k.add(KA));
        assert!(k.add(KB));
        assert_eq!(k.select(), KeyChoice::Use(KA.into()));
        assert_eq!(k.select(), KeyChoice::Use(KB.into()));
        assert_eq!(k.select(), KeyChoice::Use(KA.into()));
    }

    #[test]
    fn cle_ko_session_dead_puis_anonyme() {
        let mut k = ks();
        assert!(k.add(KA));
        assert_eq!(k.select(), KeyChoice::Use(KA.into()));
        k.note_failure(KA, ErrorKind::Exhausted, 3600);
        // plus aucune clé utilisable -> anonyme (jamais de mix clé/anonyme)
        assert_eq!(k.select(), KeyChoice::Anonymous);
        assert_eq!(k.session_dead.len(), 1);
    }

    #[test]
    fn note_success_reactive() {
        let mut k = ks();
        assert!(k.add(KA));
        k.note_failure(KA, ErrorKind::Exhausted, 3600);
        assert_eq!(k.select(), KeyChoice::Anonymous);
        k.note_success(KA);
        assert_eq!(k.select(), KeyChoice::Use(KA.into()));
    }

    #[test]
    fn cooldown_persistant_puis_expiration() {
        let mut k = ks();
        assert!(k.add(KA));
        k.note_failure(KA, ErrorKind::Exhausted, 3600);
        k.session_dead.clear(); // simule un restart
                                // toujours disabled (cooldown disque)
        assert_eq!(k.select(), KeyChoice::Anonymous);
        // expire le cooldown manuellement
        k.disabled.insert(KA.into(), ("x".into(), now_epoch()));
        assert_eq!(k.select(), KeyChoice::Use(KA.into()));
    }

    #[test]
    fn rotation_saute_les_mortes() {
        let mut k = ks();
        assert!(k.add(KA));
        assert!(k.add(KB));
        k.note_failure(KA, ErrorKind::Exhausted, 3600);
        k.session_dead.clear();
        // KA disabled, KB passe
        assert_eq!(k.select(), KeyChoice::Use(KB.into()));
        assert_eq!(k.select(), KeyChoice::Use(KB.into()));
    }

    #[test]
    fn add_remove_idempotent() {
        let mut k = ks();
        assert!(k.add(KA));
        assert!(!k.add(KA)); // doublon
        assert!(!k.add("short"));
        assert!(k.remove(KA));
        assert!(!k.remove(KA));
        assert_eq!(k.count(), 0);
    }

    #[test]
    fn save_load_roundtrip() {
        let path = {
            let dir = std::env::temp_dir().join(format!("pauvre-ks-rt-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            dir.join("keys.json")
        };
        let _ = std::fs::remove_file(&path);
        let mut k = KeyStore::open(path.clone());
        assert!(k.add("sk-roundtrip1234567890"));
        k.note_failure("sk-roundtrip1234567890", ErrorKind::Exhausted, 120);
        k.save().unwrap();
        let k2 = KeyStore::open(path.clone());
        assert_eq!(k2.count(), 1);
        assert!(k2.disabled.contains_key("sk-roundtrip1234567890"));
        assert_eq!(k2.active_count(now_epoch() + 999), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn meta_ne_divulgue_jamais_les_valeurs() {
        let mut k = ks();
        assert!(k.add("sk-secretsecretsecret12"));
        let m = serde_json::to_string(&k.meta()).unwrap();
        assert!(!m.contains("sk-secretsecretsecret12"));
        assert!(m.contains("\"total\":1"));
        assert!(m.contains("\"mode\":\"pool\""));
    }
}
