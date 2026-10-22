//! Registre des modèles + état de santé/cooldown + cascade de fallback.

use crate::classify::ErrorKind;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Durées de cooldown par type d'erreur (backoff x2 à chaque récidive).
pub const COOLDOWN_EXHAUSTED: Duration = Duration::from_secs(120);
pub const COOLDOWN_MODEL_INVALID: Duration = Duration::from_secs(3600); // quasi définitif
pub const COOLDOWN_TRANSIENT: Duration = Duration::from_secs(10);
pub const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// Modèles frontière retenus (les meilleurs gratuits testés en live).
///
/// Rotation du 22 oct 2026 (test de liveness réel, `opencode run
/// --format json` sur chaque modèle) :
/// - `longcat-2.5-preview-free` : **MORT** — « UnknownError /
///   Unexpected server error » (gateway) ;
/// - `minimax-m3.1-flash-free` : **MORT** — même erreur gateway ;
/// - `flint-40b-free` : **MORT** — « Upstream request failed: Endpoint
///   is unavailable » (503) ;
/// - `jev-1.13-free` : **MORT** — même erreur gateway ;
/// - `beam-free` : **instable** — timeout récurrent (> 90 s).
///
/// Remplacés par `big-pickle` (rapide en texte) et
/// `nemotron-3.5-lightning-free`.
///
/// ## Ordre de la chaîne = ordre de FIABILITÉ
///
/// Le 1er modèle est celui qui sert le mieux, parce qu'un modèle qui
/// HANG en mode agent brûle tout son timeout (240 s) avant que la
/// cascade passe au suivant — le client desktop attend alors 4-6 min
/// pour une tâche de 10 s. Observé : `big-pickle` a hang 240 s puis
/// `glm-5.3-flash-free` a répondu en 119 s → 359 s pour l'utilisateur.
///
/// Ordre mesuré en prod (latence + taux d'échec) :
/// `glm-5.3-flash-free` (6 ok / 0 ko, ~6 s) en tête, puis les autres
/// par fiabilité décroissante. `big-pickle` est rapide en texte mais
/// instable en agent → il passe en fin de chaîne.
pub const FREE_MODELS: &[&str] = &[
    "glm-5.3-flash-free",
    "deepseek-v4.1-flash-free",
    "mimo-v2.6-flash-free",
    "nemotron-3.5-lightning-free",
    "big-pickle",
];

/// Pseudo-modèle exposé aux clients : round-robin permanent sur les
/// frontières. Chaque requête part sur le modèle suivant — répartition
/// de charge maximale, indépendant du mode global.
pub const ROBIN_MODEL: &str = "frontier-robin-free";

/// État de santé d'un modèle.
#[derive(Debug, Clone, Default)]
pub struct Health {
    pub cooldown_until: Option<Instant>,
    pub failures: u32,
    pub ok_count: u32,
    /// Dernière latence observée (ms).
    pub last_latency_ms: u64,
    /// Latence EWMA (ms) — moyenne mobile exponentielle, alpha 1/4 :
    /// lisse les pics tout en réagissant à ~4 requêtes. 0 = jamais vu.
    pub ewma_latency_ms: f64,
    /// Nombre de fois où ce modèle a dépassé SLOW_MS (pour le scoring).
    pub slow_count: u32,
}

/// Au-delà de cette latence, un modèle est « lent » (compte dans le score).
pub const SLOW_MS: u64 = 20_000;

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
            // flag IP : pause courte côté modèle — le circuit anti-ban
            // impose déjà l'arrêt global (antiban::FLAGGED_PAUSE)
            ErrorKind::IpFlagged => Duration::from_secs(90),
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
        let ms = latency.as_millis() as f64;
        // EWMA alpha=0.25 : 0 -> seed direct, sinon lissage
        self.ewma_latency_ms = if self.ewma_latency_ms == 0.0 {
            ms
        } else {
            self.ewma_latency_ms * 0.75 + ms * 0.25
        };
        self.last_latency_ms = latency.as_millis() as u64;
        if self.last_latency_ms > SLOW_MS {
            self.slow_count = self.slow_count.saturating_add(1);
        }
        self.failures = 0;
        self.ok_count += 1;
        self.cooldown_until = None;
    }

    /// Score de préférence : plus PETIT = meilleur.
    /// EWMA de latence + 30 s par échec consécutif + 5 s par dépassement
    /// lent. Les modèles jamais vus reçoivent la médiane des connus (ou
    /// 10 s par défaut) : ils sont essayés mais ne dominent pas a priori.
    pub fn score(&self, median_ms: f64) -> f64 {
        let base = if self.ewma_latency_ms > 0.0 {
            self.ewma_latency_ms
        } else {
            median_ms
        };
        base + self.failures as f64 * 30_000.0 + self.slow_count as f64 * 5_000.0
    }
}

/// Registre partagé (verrouillé par le serveur, pas de Mutex interne ici).
pub type Registry = HashMap<String, Health>;

/// La cascade : modèle demandé en premier, puis les frontières.
/// Le pseudo-modèle `frontier-robin-free` route sur TOUTE la cascade.
pub fn cascade(requested: &str) -> Vec<String> {
    if requested == ROBIN_MODEL {
        return FREE_MODELS.iter().map(|m| (*m).to_string()).collect();
    }
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

/// Round-robin : part de `start` dans la cascade, retourne le premier
/// modèle disponible, et avance le curseur (répartit la charge).
pub fn pick_round_robin(
    cascade: &[String],
    registry: &Registry,
    start: &mut usize,
    now: Instant,
) -> Option<String> {
    let n = cascade.len();
    for i in 0..n {
        let idx = (start.wrapping_add(i)) % n;
        let m = &cascade[idx];
        if registry.get(m).map(|h| h.available(now)).unwrap_or(true) {
            *start = (idx + 1) % n;
            return Some(m.clone());
        }
    }
    None
}

/// Médiane des latences EWMA connues (fallback 10 s si tout est vierge).
fn median_known_latency(registry: &Registry, cascade: &[String]) -> f64 {
    let mut v: Vec<f64> = cascade
        .iter()
        .filter_map(|m| registry.get(m))
        .map(|h| h.ewma_latency_ms)
        .filter(|x| *x > 0.0)
        .collect();
    if v.is_empty() {
        return 10_000.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// Robin INTELLIGENT : exploite les scores (EWMA + pénalités) pour
/// envoyer la requête au modèle le plus rapide fiable, tout en gardant
/// une exploration déterministe (1 requête sur 5 part sur le meilleur
/// score PAR le modèle suivant) pour rafraîchir les mesures — un modèle
/// redevenu rapide est redétecté en ≤ 5 requêtes, un modèle dégradé est
/// évité dès la suivante. Le curseur n'avance QUE sur l'exploration :
/// l'exploitation peut frapper le même modèle plusieurs fois de suite.
/// Retourne (modèle, timeout_effectif) — le timeout s'adapte à l'EWMA :
/// rapide connu = coupe court, lent connu = marge élargie.
pub fn pick_robin_smart(
    cascade: &[String],
    registry: &Registry,
    exploration_cursor: &mut usize,
    now: Instant,
    base_timeout: Duration,
) -> Option<(String, Duration)> {
    let median = median_known_latency(registry, cascade);
    let candidates: Vec<&String> = cascade
        .iter()
        .filter(|m| registry.get(*m).map(|h| h.available(now)).unwrap_or(true))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Exploitation : min score (tie-break : ordre de cascade).
    let exploit = candidates
        .iter()
        .enumerate()
        .min_by(|(i, a), (j, b)| {
            let sa = registry
                .get(a.as_str())
                .map(|h| h.score(median))
                .unwrap_or(median);
            let sb = registry
                .get(b.as_str())
                .map(|h| h.score(median))
                .unwrap_or(median);
            sa.partial_cmp(&sb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(i.cmp(j))
        })
        .map(|(_, m)| (*m).clone())
        .unwrap_or_else(|| candidates[0].clone());
    // Exploration 1/5 : la 5e requête (cursor % 5 == 4) part sur un
    // AUTRE modèle — c'est la freshness des mesures (un modèle redevenu
    // rapide est redétecté en ≤ 5 requêtes).
    //
    // Piège corrigé : l'index d'exploration était `cursor % len` APRÈS
    // incrément. Avec 5 modèles, 5 % 5 == 0 → l'exploration retombait
    // TOUJOURS sur le premier modèle, qui est justement celui que
    // l'exploitation choisit (tie-break à score égal) : l'exploration
    // n'explorait donc RIEN (observé : 9 requêtes, 9× le même modèle).
    //
    // On indexe sur le nombre d'explorations (cursor / 5) et on SAUTE
    // l'index si il tombe sur le modèle d'exploitation : une
    // exploration qui n'explore pas est une exploration inutile.
    let explore = *exploration_cursor % 5 == 4;
    *exploration_cursor = exploration_cursor.wrapping_add(1);
    let chosen = if explore && candidates.len() > 1 {
        let mut idx = (*exploration_cursor / 5) % candidates.len();
        if candidates[idx] == &exploit {
            idx = (idx + 1) % candidates.len();
        }
        candidates[idx].clone()
    } else {
        exploit
    };
    // timeout adaptatif : 1,5× EWMA — borné [30 s, base] en prod ; si
    // base < 30 s (config de test), borné [base/3, base]. clamp(min>max)
    // panique : min est toujours <= max ici.
    let ewma = registry
        .get(&chosen)
        .map(|h| h.ewma_latency_ms)
        .unwrap_or(0.0);
    let adaptive = if ewma > 0.0 {
        let want = Duration::from_millis((ewma * 1.5) as u64);
        if base_timeout >= Duration::from_secs(30) {
            want.clamp(Duration::from_secs(30), base_timeout)
        } else {
            want.clamp(base_timeout / 3, base_timeout)
        }
    } else {
        base_timeout
    };
    Some((chosen, adaptive))
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
        "ewma_latency_ms": h.ewma_latency_ms as u64,
        "slow_count": h.slow_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h_ok(lat_ms: u64) -> Health {
        let mut h = Health::default();
        h.note_success(Duration::from_millis(lat_ms));
        h
    }

    #[test]
    fn ewma_lisse_et_reagit() {
        let mut h = Health::default();
        h.note_success(Duration::from_millis(4000));
        assert_eq!(h.ewma_latency_ms, 4000.0); // seed
        h.note_success(Duration::from_millis(8000));
        // 4000*0.75 + 8000*0.25 = 5000
        assert_eq!(h.ewma_latency_ms, 5000.0);
        h.note_success(Duration::from_millis(8000));
        assert_eq!(h.ewma_latency_ms, 5750.0); // 5000*.75+8000*.25
    }

    #[test]
    fn score_penalise_echecs_et_lenteur() {
        let rapide = h_ok(5000);
        let mut lent = h_ok(30000); // slow_count=1 (30s > 20s)
        lent.slow_count = 2;
        let mut casse = Health::default();
        casse.note_success(Duration::from_millis(5000));
        casse.note_failure(ErrorKind::Transient, Instant::now()); // failures=1
        let m = 10000.0;
        assert!(rapide.score(m) < casse.score(m), "cassé doit scorer pire");
        assert!(rapide.score(m) < lent.score(m), "lent doit scorer pire");
    }

    #[test]
    fn jamais_vu_recueit_la_mediane() {
        let mut reg: Registry = HashMap::new();
        reg.insert("a".into(), h_ok(4000));
        // b jamais vu
        let median = median_known_latency(&reg, &["a".into(), "b".into()]);
        assert_eq!(median, 4000.0);
        let h = Health::default();
        assert_eq!(h.score(median), 4000.0); // b = médiane, ni mieux ni pire
    }

    #[test]
    fn robin_smart_exploite_le_plus_rapide() {
        let now = Instant::now();
        let mut reg: Registry = HashMap::new();
        // Noms de modèles factices : le test porte sur le SCORING, pas
        // sur la rotation réelle des modèles (qui change chaque
        // semaine). Le « rapide » est le 1er de la chaîne.
        let chain = vec![
            "modele-rapide".to_string(),
            "modele-lent".to_string(),
            "modele-moyen".to_string(),
        ];
        reg.insert("modele-rapide".into(), h_ok(5000));
        reg.insert("modele-lent".into(), h_ok(25000)); // slow
        reg.insert("modele-moyen".into(), h_ok(5700));
        let mut cursor = 0usize;
        // 4 exploitations consécutives (pas d'exploration aux idx 0-3) :
        // toutes doivent tomber sur le plus rapide (score min)
        for _ in 0..4 {
            let (m, t) =
                pick_robin_smart(&chain, &reg, &mut cursor, now, Duration::from_secs(80)).unwrap();
            assert_eq!(m, "modele-rapide");
            // timeout adaptatif : 1,5× 5000 = 7,5 s, borné bas à 30 s
            assert_eq!(t, Duration::from_secs(30));
        }
    }

    #[test]
    fn robin_smart_exploration_explore_un_autre_modele() {
        // RÉGRESSION : avec 5 modèles, l'index d'exploration
        // (`cursor % len` après incrément) retombait TOUJOURS sur le
        // premier — le même que l'exploitation choisit → l'exploration
        // n'explorait rien (9 requêtes, 9× le même modèle).
        let now = Instant::now();
        let reg: Registry = HashMap::new();
        let chain: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
        let mut cursor = 0usize;
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..12 {
            let (m, _) =
                pick_robin_smart(&chain, &reg, &mut cursor, now, Duration::from_secs(80)).unwrap();
            seen.push(m);
        }
        let distinct: std::collections::HashSet<&String> = seen.iter().collect();
        assert!(
            distinct.len() >= 2,
            "l'exploration doit toucher un autre modèle: {seen:?}"
        );
    }

    #[test]
    fn robin_smart_degrade_le_lent_dans_le_timeout() {
        let now = Instant::now();
        let mut reg: Registry = HashMap::new();
        reg.insert("beam-free".into(), h_ok(5000));
        let mut slow = h_ok(40000); // > SLOW_MS
        slow.slow_count = 1;
        reg.insert("glm-5.3-flash-free".into(), slow);
        let chain = vec!["beam-free".to_string(), "glm-5.3-flash-free".to_string()];
        let mut cursor = 0usize;
        // exploration (cursor 4e appel) peut tomber sur glm → timeout
        // adaptatif = 1,5× 40 s = 60 s, pas 80 s (base) ni 30 s (min)
        let mut saw_adaptive = false;
        for _ in 0..5 {
            let (m, t) =
                pick_robin_smart(&chain, &reg, &mut cursor, now, Duration::from_secs(80)).unwrap();
            if m == "glm-5.3-flash-free" {
                assert_eq!(t, Duration::from_secs(60), "1,5× EWMA 40s = 60s");
                saw_adaptive = true;
            }
        }
        assert!(saw_adaptive, "l'exploration doit passer par glm (1/5)");
    }

    #[test]
    fn robin_smart_tout_en_cooldown_none() {
        let now = Instant::now();
        let mut reg: Registry = HashMap::new();
        let mut h = Health::default();
        h.note_failure(ErrorKind::Exhausted, now);
        reg.insert("beam-free".into(), h);
        let chain = vec!["beam-free".to_string()];
        let mut cursor = 0usize;
        assert!(
            pick_robin_smart(&chain, &reg, &mut cursor, now, Duration::from_secs(80)).is_none()
        );
    }

    #[test]
    fn cascade_demande_dabord_puis_frontiere() {
        let c = cascade("deepseek-v4.1-flash-free");
        assert_eq!(c[0], "deepseek-v4.1-flash-free");
        assert!(c.contains(&"glm-5.3-flash-free".to_string()));
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
        // Le nom du modèle demandé est libre : ce qui compte, c'est
        // que le 1er en cooldown soit sauté pour le 1er DISPONIBLE de
        // la chaîne (le test ne doit pas dépendre de la rotation des
        // modèles, qui change chaque semaine).
        let c = cascade("modele-inexistant-xyz");
        let expected = FREE_MODELS
            .iter()
            .find(|m| **m != "modele-inexistant-xyz")
            .expect("cascade non vide");
        let mut h = Health::default();
        h.note_failure(ErrorKind::Exhausted, now);
        reg.insert("modele-inexistant-xyz".into(), h);
        assert_eq!(pick(&c, &reg, now).unwrap(), *expected);
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
    fn round_robin_repartit_la_charge() {
        let now = Instant::now();
        let reg: Registry = HashMap::new();
        let c = cascade("space-bunny-free");
        let mut idx = 0usize;
        // départ sur glm (index 1 de la chaîne space-bunny), puis mimo, etc.
        let a = pick_round_robin(&c, &reg, &mut idx, now).unwrap();
        let b = pick_round_robin(&c, &reg, &mut idx, now).unwrap();
        let cc = pick_round_robin(&c, &reg, &mut idx, now).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, cc);
        // skip des cooldowns : glm en cooldown → passe au suivant
        let mut reg2: Registry = HashMap::new();
        let mut h2 = Health::default();
        h2.note_failure(ErrorKind::Exhausted, now);
        reg2.insert("glm-5.3-flash-free".into(), h2);
        let mut idx2 = 1usize;
        let next = pick_round_robin(&c, &reg2, &mut idx2, now).unwrap();
        assert_ne!(next, "glm-5.3-flash-free");
    }

    #[test]
    fn robin_route_sur_toutes_les_frontieres() {
        let c = cascade("frontier-robin-free");
        assert_eq!(
            c,
            FREE_MODELS
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
        );
        // sanity : robin est bien le pseudo-modèle déclaré
        assert_eq!(ROBIN_MODEL, "frontier-robin-free");
    }

    #[test]
    fn model_invalide_cooldown_long() {
        let now = Instant::now();
        let mut h = Health::default();
        let cd = h.note_failure(ErrorKind::ModelInvalid, now);
        assert_eq!(cd, COOLDOWN_MODEL_INVALID);
    }
}
