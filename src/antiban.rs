//! Protection anti-ban IP — la leçon de l'incident du 18 oct 2026.
//!
//! L'amont (gateway `opencode.ai/zen/v1`) flague l'IP pour « abusive
//! traffic » quand le volume d'appels automatisés dérape (incident réel :
//! ~800 messages/jour en anonyme → ban des IPs, VPN ET maison, pendant
//! ~24 h). L'anti-spam existant [`crate::guard::RateLimiter`] protège par
//! IP *cliente* ; rien ne bornait le flux TOTAL vers l'amont. Ce module
//! ajoute deux étages :
//!
//! 1. **Budget global** : plafond d'appels backend par fenêtre glissante
//!    (prod : 30/min, env `PAUVRE_BUDGET`, plancher 6). Au-delà → 429
//!    immédiat au client, AUCUN spawn.
//! 2. **Circuit breaker** : la détection du message « abusive traffic »
//!    (voir [`crate::classify::is_ip_flagged`]) ouvre un circuit 10 min —
//!    tout appel backend est refusé sans spawn (chaque essai renforce
//!    probablement le flag). Pendant le flag, un cooldown amont prolonge
//!    la coupure (les 4 modèles partagent la même gateway : la cascade
//!    est vouée à l'échec). Un succès backend referme le circuit.
//!
//! L'état flag/circuit est volatil (non sérialisé) : au reboot, la
//! première réponse amont re-déclenche la détection si le ban persiste.

use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Fenêtre glissante du budget.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Budget par défaut : 30 appels backend/min. L'incident a déclenché le
/// flag vers ~120 appels/20 min (et le free tier n'est de toute façon pas
/// fait pour 800+ messages/jour) : on tient 4× sous le seuil observé.
pub const DEFAULT_BUDGET: usize = 30;
/// Plancher du budget : 6 appels/min en backoff maximal. Le budget est
/// utile comme garde-fou, pas pour couper un service déjà prudent.
pub const MIN_BUDGET: usize = 6;
/// Durée du circuit ouvert après détection d'un flag IP. 10 min : assez
/// long pour ne PAS marteler une gateway qui nous a flaggé, assez court
/// pour retomber sur ses pieds si le flag était bref.
pub const FLAGGED_PAUSE: Duration = Duration::from_secs(600);

/// État partagé anti-ban (thread-safe, verrous fins).
pub struct AntiBan {
    budget: RwLock<usize>,
    /// Appels backend de la fenêtre glissante (plus ancien en tête).
    calls: Mutex<VecDeque<Instant>>,
    /// Jusqu'à quand le circuit est ouvert (flag détecté).
    blocked_until: RwLock<Option<Instant>>,
}

impl AntiBan {
    pub fn new(budget: usize) -> Self {
        Self {
            budget: RwLock::new(budget.max(MIN_BUDGET)),
            calls: Mutex::new(VecDeque::new()),
            blocked_until: RwLock::new(None),
        }
    }

    /// Budget effectif (après plancher).
    pub fn budget(&self) -> usize {
        *self.budget.read().unwrap()
    }

    /// Change le budget à chaud (admin) — planché à [`MIN_BUDGET`].
    /// Retourne la valeur appliquée.
    pub fn set_budget(&self, want: usize) -> usize {
        let mut b = self.budget.write().unwrap();
        *b = want.max(MIN_BUDGET);
        *b
    }

    /// Consomme un jeton d'appel backend — ou renvoie (Err secondes) le
    /// délai d'attente. Variante testable à instant imposé.
    pub fn acquire(&self) -> Result<(), u64> {
        self.acquire_at(Instant::now())
    }

    pub fn acquire_at(&self, now: Instant) -> Result<(), u64> {
        // 1) circuit ouvert ? → refus sec, aucune consommation
        if let Some(wait) = self.blocked_wait(now) {
            return Err(wait);
        }
        // 2) budget fenêtre glissante
        let mut calls = self.calls.lock().unwrap();
        while let Some(front) = calls.front() {
            if now.duration_since(*front) >= WINDOW {
                calls.pop_front();
            } else {
                break;
            }
        }
        if calls.len() >= self.budget() {
            let front = *calls.front().unwrap();
            let wait = (WINDOW - now.duration_since(front)).as_secs().max(1);
            return Err(wait);
        }
        calls.push_back(now);
        Ok(())
    }

    /// Flag détecté : circuit ouvert [`FLAGGED_PAUSE`].
    pub fn flag(&self) {
        self.flag_at(Instant::now());
    }

    pub fn flag_at(&self, now: Instant) {
        *self.blocked_until.write().unwrap() = Some(now + FLAGGED_PAUSE);
    }

    /// Pendant un flag, un cooldown amont `secs` prolonge la coupure
    /// (2× le cooldown, plafonné à FLAGGED_PAUSE) — la gateway dit
    /// « réessaie dans X » : on ne réouvre pas avant. Hors flag : no-op
    /// (le budget suffit hors incident).
    pub fn cooldown_changed(&self, secs: u64) {
        self.cooldown_changed_at(Instant::now(), secs);
    }

    pub fn cooldown_changed_at(&self, now: Instant, secs: u64) {
        let mut b = self.blocked_until.write().unwrap();
        if let Some(t) = *b {
            if now < t {
                let want = now + Duration::from_secs((secs * 2).max(60));
                let cap = now + FLAGGED_PAUSE;
                if want > t {
                    *b = Some(want.min(cap));
                }
            }
        }
    }

    /// Succès backend : referme le circuit et purge le flag.
    pub fn release(&self) {
        *self.blocked_until.write().unwrap() = None;
    }

    pub fn is_flagged(&self) -> bool {
        self.is_flagged_at(Instant::now())
    }

    pub fn is_flagged_at(&self, now: Instant) -> bool {
        match *self.blocked_until.read().unwrap() {
            Some(t) => now < t,
            None => false,
        }
    }

    /// (appels dans la fenêtre, flag actif, secondes de circuit restantes).
    pub fn status(&self) -> (usize, bool, u64) {
        self.status_at(Instant::now())
    }

    pub fn status_at(&self, now: Instant) -> (usize, bool, u64) {
        let n = self.calls.lock().unwrap().len();
        let blocked = self.blocked_wait(now);
        (n, blocked.is_some(), blocked.unwrap_or(0))
    }

    fn blocked_wait(&self, now: Instant) -> Option<u64> {
        match *self.blocked_until.read().unwrap() {
            Some(t) if now < t => Some((t - now).as_secs().max(1)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_bloque_puis_se_recycle() {
        let ab = AntiBan::new(3); // demandé 3 → planché à MIN_BUDGET = 6
        assert_eq!(ab.budget(), MIN_BUDGET);
        let t0 = Instant::now();
        for _ in 0..MIN_BUDGET {
            assert!(ab.acquire_at(t0).is_ok());
        }
        let wait = ab.acquire_at(t0).unwrap_err();
        assert!(wait >= 1, "appel au-delà du budget doit être refusé");
        assert_eq!(ab.status_at(t0).0, MIN_BUDGET);
        // fenêtre écoulée : les anciens appels sortent, ça repasse
        assert!(ab.acquire_at(t0 + WINDOW).is_ok());
    }

    #[test]
    fn flag_ouvre_le_circuit_sans_consommer() {
        let ab = AntiBan::new(10);
        let t0 = Instant::now();
        assert!(ab.acquire_at(t0).is_ok());
        ab.flag_at(t0);
        let (n, flagged, blocked) = ab.status_at(t0);
        assert_eq!(n, 1, "le flag ne touche pas le compteur de budget");
        assert!(flagged);
        assert!(blocked > 500, "circuit ~600s restantes, pas {blocked}s");
        let wait = ab.acquire_at(t0 + Duration::from_secs(30)).unwrap_err();
        assert!(wait > 500);
        // un succès referme tout
        ab.release();
        assert!(!ab.is_flagged());
        assert!(ab.acquire_at(t0 + Duration::from_secs(31)).is_ok());
    }

    #[test]
    fn cooldown_prolonge_le_flag_sans_le_depasser() {
        let ab = AntiBan::new(10);
        let t0 = Instant::now();
        ab.flag_at(t0); // circuit jusqu'à t0+600
        let t1 = t0 + Duration::from_secs(10);
        ab.cooldown_changed_at(t1, 120); // amont dit 120s → veut t1+240 < cap
        let (_, _, blocked) = ab.status_at(t1);
        assert_eq!(
            blocked, 590,
            "t0+600 - t1(10s écoulées) : le prolongement (240s) < reste (590s), inchangé"
        );
        // cooldown énorme : le prolongement est plafonné à FLAGGED_PAUSE
        // À PARTIR DE MAINTENANT (t1+600) — auto-limitant : pendant un long
        // ban, le circuit se rouvre toutes les 10 min pour UNE sonde, le
        // volume reste négligeable.
        ab.cooldown_changed_at(t1, 5000);
        let (_, _, blocked2) = ab.status_at(t1);
        assert_eq!(blocked2, 600, "cap = t1+FLAGGED_PAUSE");
        // hors flag : no-op
        ab.release();
        ab.cooldown_changed_at(t1, 120);
        assert!(!ab.is_flagged_at(t1));
    }

    #[test]
    fn plancher_budget_respecte() {
        let ab = AntiBan::new(1); // demandé 1 → planché à MIN_BUDGET
        assert_eq!(ab.budget(), MIN_BUDGET);
    }
}
