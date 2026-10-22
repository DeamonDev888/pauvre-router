//! Détection de mise à jour — compare la version du binaire à la
//! dernière release GitHub. Zéro dépendance : HTTPS via le client
//! `curl` présent sur Windows (win7+), macOS et la quasi-totalité des
//! Linux ; absent → statut "unknown", jamais d'erreur exposée.
//!
//! Comparaison semver maison (major.minor.patch, numériques) : un tag
//! non conforme est ignoré. Le téléchargement n'est PAS fait ici — on
//! renvoie l'URL de la release, c'est l'opérateur (ou `/admin/update`
//! côté client) qui décide.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Périodique : check une fois par heure au maximum.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(3600);
/// Check réseau : 6 s max, on est paspressés.
const CURL_TIMEOUT: Duration = Duration::from_secs(6);
const LATEST_URL: &str = "https://api.github.com/repos/DeamonDev888/pauvre-router/releases/latest";

/// Résultat du dernier check connu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateStatus {
    /// Version à jour (ce binaire).
    pub current: String,
    /// Dernière release publiée, si le check a réussi.
    pub latest: Option<String>,
    /// true si `latest > current` (semver).
    pub update_available: bool,
    /// URL de la release à télécharger.
    pub url: String,
    /// Instant (epoch) du dernier check réussi, 0 = jamais.
    pub checked_at_epoch: u64,
}

pub struct Updater {
    pub current: String,
    status: Mutex<UpdateStatus>,
    /// Check en cours ? (un seul à la fois)
    inflight: AtomicU64,
}

impl Updater {
    pub fn new(current: String) -> Self {
        let status = UpdateStatus {
            current: current.clone(),
            latest: None,
            update_available: false,
            url: "https://github.com/DeamonDev888/pauvre-router/releases/latest".into(),
            checked_at_epoch: 0,
        };
        Self {
            current,
            status: Mutex::new(status),
            inflight: AtomicU64::new(0),
        }
    }

    /// Dernier état connu (jamais bloquant).
    pub fn status(&self) -> UpdateStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Check synchrone (6 s max) : interroge l'API GitHub Releases.
    /// Ne panic jamais — en cas d'échec, le statut précédent reste ET
    /// le flag est réarmé (sinon le premier échec de réseau brickerait
    /// tous les checks suivants — bug vu en prod au boot avant réseau).
    pub fn check(&self) -> UpdateStatus {
        // un seul check à la fois
        if self.inflight.swap(1, Ordering::Relaxed) == 1 {
            return self.status();
        }
        let result = fetch_latest();
        match result {
            Some((tag, url)) => {
                let update_available = semver_gt(&tag, &self.current);
                let next = UpdateStatus {
                    current: self.current.clone(),
                    latest: Some(tag),
                    update_available,
                    url,
                    checked_at_epoch: epoch_now(),
                };
                *self.status.lock().unwrap_or_else(|e| e.into_inner()) = next.clone();
                self.inflight.store(0, Ordering::Relaxed);
                next
            }
            None => {
                // réseau KO : on garde l'ancien statut MAIS on réarme,
                // le prochain check (horaire ou forcé) doit pouvoir partir
                self.inflight.store(0, Ordering::Relaxed);
                self.status()
            }
        }
    }

    /// Boucle de fond : check au boot puis toutes les CHECK_INTERVAL.
    /// Tourne dans un thread détaché (le serveur vit pour toujours).
    pub fn spawn_periodic(self: &Arc<Self>) {
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("pauvre-updater".into())
            .spawn(move || {
                me.check();
                loop {
                    std::thread::sleep(CHECK_INTERVAL);
                    me.check();
                }
            })
            .ok();
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Interroge l'API GitHub. Retourne (tag, url_de_release) ou None.
fn fetch_latest() -> Option<(String, String)> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-m",
            &CURL_TIMEOUT.as_secs().to_string(),
            "-H",
            "User-Agent: pauvre-router-updater",
            "-H",
            "Accept: application/vnd.github+json",
            LATEST_URL,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let tag = doc["tag_name"]
        .as_str()?
        .trim_start_matches('v')
        .to_string();
    if tag.is_empty() {
        return None;
    }
    let url = doc["html_url"]
        .as_str()
        .unwrap_or("https://github.com/DeamonDev888/pauvre-router/releases/latest")
        .to_string();
    Some((tag, url))
}

/// semver a > b ? (major.minor.patch numériques ; préfixes v/whitespace tolérés ;
/// tout tag non conforme → false, jamais de panic)
pub fn semver_gt(a: &str, b: &str) -> bool {
    match (parse_semver(a), parse_semver(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next()?.parse().ok()?;
    let pat = it.next()?.parse().ok()?;
    Some((maj, min, pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparaisons() {
        assert!(semver_gt("0.5.2", "0.5.1"));
        assert!(semver_gt("v0.6.0", "0.5.1"));
        assert!(semver_gt("1.0.0", "0.99.99"));
        assert!(!semver_gt("0.5.1", "0.5.1"));
        assert!(!semver_gt("0.5.0", "0.5.1"));
        assert!(!semver_gt("pas-un-tag", "0.5.1"));
        assert!(!semver_gt("", "0.5.1"));
        assert!(!semver_gt("1.2", "0.5.1")); // incomplet = ignoré
    }

    #[test]
    fn updater_etat_initial() {
        let u = Updater::new("0.5.1".into());
        let s = u.status();
        assert_eq!(s.current, "0.5.1");
        assert!(!s.update_available);
        assert_eq!(s.checked_at_epoch, 0);
    }
}
