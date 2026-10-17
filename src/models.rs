//! Registre des modèles + état de santé/cooldown + cascade de fallback.

use crate::classify::ErrorKind;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Durées de cooldown par type d'erreur (backoff x2 à chaque récidive).
pub const COOLDOWN_EXHAUSTED: Duration = Duration::from_secs(120);
pub const COOLDOWN_MODEL_INVALID: Duration = Duration::from_secs(3600); // quasi définitif
pub const COOLDOWN_TRANSIENT: Duration = Duration::from_secs(10);
pub const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// Modèles gratuits frontière, ordre de cascade (qualité perçue décroissante).
pub const FREE_MODELS: &[&str] = &[
    "space-bunny-free",
    "mimo-v2.6-flash-free",
    "nemotron-3.5-lightning-free",
    "longcat-2.5-preview-free",
];

/// État de santé d'un modèle.
#[derive(Debug, Clone, Default)]
pub struct Health {
    pub cooldown_until: Option<Instant>,
    pub failures: u32,
    pub ok_count: u32,
    /// Dernière latence observée (ms).
    pub last_latency_ms: u64,
}

impl Health {
    pub fn available(&self, now: Instant) -> bool {
        match self.cooldown_until {
            Some(t) => now >= t,
            None => true,
        }
    }

    pub fn note_failure(&mut self, kind: ErrorKind, now: Instant) -> Duration {
        let base = match kind {
            ErrorKind::Exhausted => COOLDOWN_EXHAUSTED,
            ErrorKind::ModelInvalid => COOLDOWN_MODEL_INVALID,
            ErrorKind::Transient => COOLDOWN_TRANSIENT,
        };
        // backoff x2 par récidive consécutive, plafonné
        let mult = 1u32 << self.failures.min(6);
        let cd = base.mul_f64(mult as f64).min(MAX_BACKOFF);
        self.failures += 1;
        self.cooldown_until = Some(now + cd);
        cd
    }

    pub fn note_success(&mut self, latency: Duration) {
        self.failures = 0;
        self.ok_count += 1;
        self.cooldown_until = None;
        self.last_latency_ms = latency.as_millis() as u64;
    }
}

/// Registre partagé (verrouillé par le serveur, pas de Mutex interne ici).
pub type Registry = HashMap<String, Health>;

/// La cascade : modèle demandé en premier, puis les frontière libres.
pub fn cascade(requested: &str) -> Vec<String> {
    let mut out = vec![requested.to_string()];
    for m in FREE_MODELS {
        if *m != requested && !out.iter().any(|x| x == m) {
            out.push((*m).to_string());
        }
    }
    out
}

/// Choisit le prochain modèle tentable dans la cascade.
pub fn pick(cascade: &[String], registry: &Registry, now: Instant) -> Option<String> {
    cascade
        .iter()
        .find(|m| registry.get(*m).map(|h| h.available(now)).unwrap_or(true))
        .cloned()
}

/// Sérialise l'état de santé d'un modèle pour le dump JSON.
pub fn health_json(h: &Health, now: Instant) -> serde_json::Value {
    serde_json::json!({
        "cooldown_secs": h
            .cooldown_until
            .and_then(|t| t.checked_duration_since(now))
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "failures": h.failures,
        "ok": h.ok_count,
        "last_latency_ms": h.last_latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_demande_dabord_puis_frontiere() {
        let c = cascade("mimo-v2.6-flash-free");
        assert_eq!(c[0], "mimo-v2.6-flash-free");
        assert!(c.contains(&"space-bunny-free".to_string()));
        assert_eq!(c.len(), FREE_MODELS.len()); // pas de doublon
        let c2 = cascade("inconnu-xyz");
        assert_eq!(c2[0], "inconnu-xyz");
        assert_eq!(c2.len(), FREE_MODELS.len() + 1);
    }

    #[test]
    fn cooldown_backoff_exponentiel() {
        let now = Instant::now();
        let mut h = Health::default();
        let cd1 = h.note_failure(ErrorKind::Exhausted, now);
        assert_eq!(cd1, COOLDOWN_EXHAUSTED);
        assert!(!h.available(now));
        assert!(h.available(now + COOLDOWN_EXHAUSTED));
        let cd2 = h.note_failure(ErrorKind::Exhausted, now);
        assert_eq!(cd2, COOLDOWN_EXHAUSTED * 2);
        let cd4 = h.note_failure(ErrorKind::Exhausted, now);
        assert_eq!(cd4, COOLDOWN_EXHAUSTED * 4);
        // succès réinitialise
        h.note_success(Duration::from_millis(900));
        assert_eq!(h.failures, 0);
        assert!(h.available(now));
        assert_eq!(h.last_latency_ms, 900);
    }

    #[test]
    fn cooldown_plafonne() {
        let now = Instant::now();
        let mut h = Health::default();
        for _ in 0..10 {
            h.note_failure(ErrorKind::Transient, now);
        }
        assert!(h.available(now + MAX_BACKOFF));
        assert!(!h.available(now + Duration::from_secs(1)));
    }

    #[test]
    fn pick_saute_les_modeles_en_cooldown() {
        let now = Instant::now();
        let mut reg: Registry = HashMap::new();
        let c = cascade("space-bunny-free");
        let mut h = Health::default();
        h.note_failure(ErrorKind::Exhausted, now);
        reg.insert("space-bunny-free".into(), h);
        assert_eq!(
            pick(&c, &reg, now).unwrap(),
            "mimo-v2.6-flash-free" // saute le premier en cooldown
        );
        // tous en cooldown => None
        for m in FREE_MODELS {
            let mut h = Health::default();
            h.note_failure(ErrorKind::Exhausted, now);
            reg.insert((*m).to_string(), h);
        }
        assert!(pick(&c, &reg, now).is_none());
        // cooldown expiré => de nouveau dispo
        assert!(pick(&c, &reg, now + COOLDOWN_EXHAUSTED).is_some());
    }

    #[test]
    fn model_invalide_cooldown_long() {
        let now = Instant::now();
        let mut h = Health::default();
        let cd = h.note_failure(ErrorKind::ModelInvalid, now);
        assert_eq!(cd, COOLDOWN_MODEL_INVALID);
    }
}
