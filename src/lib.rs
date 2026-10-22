//! Router des Pauvres — bibliothèque.
//!
//! Proxy local OpenAI-compat (port 4445) qui route les requêtes chat
//! vers les modèles gratuits d'OpenCode Zen via le CLI officiel
//! `opencode run`, avec :
//! - anti-spam (token bucket, plafond de concurrence, taille de body)
//! - détection d'exhaustion + cooldowns à backoff exponentiel
//! - cascade de fallback vers les modèles frontière disponibles
//! - cache de déduplication des prompts identiques
//! - état de santé persistant entre les redémarrages
pub mod antiban;
pub mod backend;
pub mod cache;
pub mod classify;
pub mod guard;
pub mod http;
pub mod keystore;
pub mod logbuf;
pub mod models;
pub mod prompt;
pub mod server;
pub mod stats;
pub mod update;

/// Version du crate (affichée dans /health et /admin/stats).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Verrou **poison-tolérant** : on récupère le contenu même si le mutex a
/// été empoisonné par un panic survenu ailleurs.
///
/// Pourquoi : `Mutex::lock().unwrap()` PANIQUE sur un mutex empoisonné —
/// et le panic se propage à CHAQUE accès ultérieur (le mutex reste
/// empoisonné). Un seul panic dans une section critique rend donc
/// définitivement inutilisable le mutex concerné : avec `.unwrap()`,
/// `/health` (mode) ou `/admin/stats` (stats) commençaient à paniquer
/// pour de bon, et le service répondait « muet » alors qu'il tournait.
///
/// Ici on assume l'incohérence résiduelle (l'état sous le verrou peut
/// être à moitié modifié) : elle est BENIGNE pour nos structures (cache
/// LRU, registre de santé, compteurs), alors qu'un service mort ne l'est
/// pas. Le panic lui-même est isolé par [`std::panic::catch_unwind`] dans
/// la boucle des workers (voir `server::bind_server`).
pub fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`lock`] pour un `RwLock` en lecture (budget / circuit anti-ban).
pub fn rlock<T>(m: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(|e| e.into_inner())
}

/// [`lock`] pour un `RwLock` en écriture (budget / circuit anti-ban).
pub fn wlock<T>(m: &std::sync::RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(|e| e.into_inner())
}
