//! Tests d'exécution du backend — `call_at` contre le binaire stub réel
//! (compilé par cargo ; `CARGO_BIN_EXE_*` n'existe que pour les cibles
//! d'intégration, d'où ce fichier plutôt que des tests unitaires lib).

use pauvre_router::backend::{call_at, BackendError};
use pauvre_router::classify::{classify, ErrorKind};
use std::time::Duration;

fn stub() -> &'static str {
    env!("CARGO_BIN_EXE_stub-opencode")
}

#[test]
fn call_at_echo_chemin_nominal() {
    let ok = call_at(
        stub(),
        "glm-5.3-flash-free",
        "PING unit",
        Duration::from_secs(10),
        None,
        &[],
        false,
        None,
    )
    .expect("stub doit répondre");
    assert_eq!(ok.text, "ack opencode/glm-5.3-flash-free: PING unit");
    assert!(ok.elapsed > Duration::ZERO);
}

#[test]
fn call_at_echec_exhaustion_classee() {
    let env = vec![("STUB_FAIL".to_string(), "glm-5.3-flash-free".to_string())];
    let err = call_at(
        stub(),
        "glm-5.3-flash-free",
        "PING",
        Duration::from_secs(10),
        None,
        &env,
        false,
        None,
    )
    .expect_err("STUB_FAIL doit faire échouer");
    match err {
        BackendError::Failed { raw, .. } => {
            assert_eq!(classify(&raw), ErrorKind::Exhausted)
        }
        other => panic!("attendu Failed, eu {other:?}"),
    }
}

#[test]
fn call_at_timeout_sur_backend_muet() {
    let err = call_at(
        stub(),
        "m",
        "SLOW 5000",
        Duration::from_millis(300),
        None,
        &[],
        false,
        None,
    )
    .expect_err("SLOW doit dépasser le timeout");
    assert!(matches!(err, BackendError::Timeout { .. }));
}

#[test]
fn call_at_passe_la_cle_api_au_spawn() {
    // clé factice de test (construite à l'exécution, jamais une vraie)
    let k = format!("sk-unit-{}", "x".repeat(20));
    let ok = call_at(
        stub(),
        "m",
        "ECHOKEY",
        Duration::from_secs(10),
        Some(&k),
        &[],
        false,
        None,
    )
    .expect("stub doit répondre");
    assert_eq!(ok.text, format!("KEY={k} model=opencode/m"));
    // sans clé : explicitement anonyme
    let ok2 = call_at(
        stub(),
        "m",
        "ECHOKEY",
        Duration::from_secs(10),
        None,
        &[],
        false,
        None,
    )
    .unwrap();
    assert!(ok2.text.starts_with("KEY=ANONYMOUS"));
}

#[test]
fn call_at_env_additionnelle_positionnee() {
    let env = vec![(
        "STUB_FAIL".to_string(),
        "longcat-2.5-preview-free".to_string(),
    )];
    // le modèle visé est dans la liste → échec ; un autre → OK
    let err = call_at(
        stub(),
        "longcat-2.5-preview-free",
        "x",
        Duration::from_secs(10),
        None,
        &env,
        false,
        None,
    )
    .expect_err("le modèle listé doit échouer");
    assert!(matches!(err, BackendError::Failed { .. }));
    let ok = call_at(
        stub(),
        "deepseek-v4.1-flash-free",
        "x",
        Duration::from_secs(10),
        None,
        &env,
        false,
        None,
    )
    .expect("le modèle hors liste doit passer");
    assert_eq!(ok.text, "ack opencode/deepseek-v4.1-flash-free: x");
}

#[test]
fn call_at_spawn_inexistant_erreur_failed() {
    let err = call_at(
        "Z:\\n existe pas\\opencode.exe",
        "m",
        "x",
        Duration::from_secs(5),
        None,
        &[],
        false,
        None,
    )
    .expect_err("spawn impossible = Failed");
    assert!(matches!(err, BackendError::Failed { .. }));
}

/// RÉGRESSION du bug « desktop » : le journal des outils (`tool_use`)
/// ne doit JAMAIS atteindre le client — seule la réponse (`text`)
/// doit être servie. Le stub émet un `tool_use` AVANT le `text`.
#[test]
fn call_at_journal_des_outils_ne_fuit_pas() {
    let ok = call_at(
        stub(),
        "m",
        "TOOLLOG PONG",
        Duration::from_secs(10),
        None,
        &[],
        false,
        None,
    )
    .expect("stub doit répondre");
    assert_eq!(ok.text, "ack opencode/m: PONG");
    assert!(
        !ok.text.contains("SECRETLOG"),
        "le journal des outils a fuité: {}",
        ok.text
    );
}

/// L'erreur amont structurée doit atteindre le classifieur : le stub
/// émet un événement `error` JSON (et non un stderr texte).
#[test]
fn call_at_erreur_structuree_atteint_le_classifieur() {
    let err = call_at(
        stub(),
        "m",
        "COOLDOWN 42",
        Duration::from_secs(10),
        None,
        &[],
        false,
        None,
    )
    .expect_err("COOLDOWN doit faire échouer");
    match err {
        BackendError::Failed { raw, .. } => {
            assert!(raw.contains("retry after 42"), "raw={raw}");
            assert_eq!(classify(&raw), ErrorKind::Exhausted);
        }
        other => panic!("attendu Failed, eu {other:?}"),
    }
}
