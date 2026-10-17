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
pub mod backend;
pub mod cache;
pub mod classify;
pub mod guard;
pub mod http;
pub mod models;
pub mod prompt;
pub mod stats;

/// Version du crate (affichée dans /health et /admin/stats).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
