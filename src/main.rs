//! Router des Pauvres — binaire.
//!
//! Le cœur (routing, anti-spam, cascade, endpoints) vit dans
//! `pauvre_router::server` : le binaire ne fait que construire la config
//! de production et lancer la boucle d'accept. C'est ce découpage qui
//! permet aux tests E2E de démarrer le MÊME serveur sur un port éphémère
//! avec un backend stub.

use pauvre_router::server::{self, ServerConfig};

fn main() {
    let cfg = ServerConfig::production();
    let port = cfg.port;
    match server::bind_server(&cfg) {
        Ok(bound) => {
            eprintln!("[pauvre-router] config prod: port {port}");
            server::serve(bound);
        }
        Err(e) => {
            eprintln!("[pauvre-router] bind 127.0.0.1:{port} KO: {e}");
            std::process::exit(1);
        }
    }
}
