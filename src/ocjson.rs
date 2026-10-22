//! Extraction de la VRAIE réponse opencode depuis la sortie du CLI.
//!
//! ## Pourquoi ce module existe
//!
//! `opencode run` (format `default`) écrit sur **deux** flux :
//! - **stdout** : la réponse finale du modèle ;
//! - **stderr** : la bannière (`> build · <modèle>`) ET la transcription
//!   des appels d'outils (`$ echo … && ls src` + la sortie de la
//!   commande).
//!
//! En mode **agent** (le client déclare des tools → opencode tourne en
//! agent `build`), la réponse et la transcription sortent MÊLÉES : le
//! v0.8.3 a donc fusionné stdout+stderr en UNE « transcription » pour
//! ne pas perdre la réponse. Effet de bord observé en prod : le client
//! (Hermes desktop) recevait le **journal des outils** au lieu de la
//! réponse — `HELLO_FROM_AGENT\n\nExécution réussie…` — et la
//! bannière pouvait réapparaître en fin de réponse (corrigé en v0.8.8
//! par `strip_banner_lines`).
//!
//! La sortie `default` est donc **ambiguë** : impossible de distinguer
//! « la réponse » de « le journal » de façon fiable. C'est pour ça que
//! `strip_banner` / `strip_banner_lines` (v0.8.3-v0.8.8) ont été
//! supprimés : ils ne faisaient que PANSER le symptôme.
//!
//! ## La solution : `--format json`
//!
//! `opencode run --format json` émet une **ligne JSON par événement** :
//!
//! ```text
//! {"type":"step_start", …}
//! {"type":"tool_use","part":{"type":"tool","state":{"output":"…"}}}
//! {"type":"text","part":{"type":"text","text":"LA RÉPONSE"}}
//! {"type":"step_finish","part":{"reason":"stop"}}
//! {"type":"error","error":{"name":"UnknownError","data":{"message":"…"}}}
//! ```
//!
//! On ne garde que le champ `text` des événements `type == "text"` :
//! c'est **exactement** ce que l'agent a dit au client, sans le
//! journal des outils. Les événements `tool_use` sont ignorés (ils ne
//! portent pas de `text`).
//!
//! Le mode `text` (client sans tools) et le mode `agent` (client avec
//! tools) utilisent le MÊME chemin : un seul parseur, un seul format.
//! On garde `--format json` partout — c'est aussi ce qui rend les
//! erreurs amont (« Unexpected server error ») **lisibles** au
//! classifieur au lieu d'être noyées dans 28 Ko de transcript.

use serde_json::Value;

/// Résultat du parsing d'une sortie `opencode --format json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Réponse texte du modèle (une ou plusieurs parties concaténées).
    Text(String),
    /// Erreur amont structurée (événement `type == "error"`).
    Error(String),
    /// Sortie sans événement exploitable (ni texte ni erreur).
    Empty,
}

impl Parsed {
    /// Texte à servir au client, ou None si c'est une erreur.
    pub fn text(&self) -> Option<&str> {
        match self {
            Parsed::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// Parse une sortie `opencode --format json` (une ligne JSON par
/// événement). Tolère :
/// - des lignes non-JSON (journaux opencode mêlés) → ignorées ;
/// - un JSON invalide → ignoré ;
/// - plusieurs événements `text` → concaténés (le modèle peut parler
///   en plusieurs parts).
///
/// Un événement `error` prend le dessus : une erreur amont doit
/// remonter au classifieur comme une erreur, pas comme un texte vide.
pub fn parse_events(raw: &str) -> Parsed {
    let mut text = String::new();
    let mut error: Option<String> = None;
    for line in raw.lines() {
        let l = line.trim();
        if !l.starts_with('{') {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        match ev.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = ev
                    .get("part")
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(t);
                }
            }
            Some("error") if error.is_none() => {
                error = Some(error_message(&ev));
            }
            _ => {}
        }
    }
    if let Some(e) = error {
        return Parsed::Error(e);
    }
    let t = text.trim();
    if t.is_empty() {
        Parsed::Empty
    } else {
        Parsed::Text(t.to_string())
    }
}

/// Extrait le message d'erreur d'un événement `type == "error"`.
/// Forme réelle d'opencode :
/// `{"type":"error","error":{"name":"UnknownError","data":{"message":"…","ref":"…"}}}`
fn error_message(ev: &Value) -> String {
    let err = ev.get("error");
    let msg = err
        .and_then(|e| e.get("data"))
        .and_then(|d| d.get("message"))
        .and_then(Value::as_str)
        .or_else(|| err.and_then(|e| e.get("message")).and_then(Value::as_str));
    match msg {
        Some(m) => {
            let name = err
                .and_then(|e| e.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("Error");
            format!("{name}: {m}")
        }
        None => err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "erreur opencode inconnue".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texte_simple() {
        let raw = r#"{"type":"text","part":{"type":"text","text":"OK"}}"#;
        assert_eq!(parse_events(raw), Parsed::Text("OK".into()));
    }

    #[test]
    fn ignore_le_journal_des_outils() {
        // cas réel de prod : le transcript des outils NE DOIT PAS fuiter
        let raw = concat!(
            r#"{"type":"step_start","timestamp":1}"#,
            "\n",
            r#"{"type":"tool_use","part":{"type":"tool","tool":"bash","state":{"status":"completed","input":{"command":"echo MARKER_XYZ && ls src"},"output":"MARKER_XYZ\nantiban.rs\n"}}}"#,
            "\n",
            r#"{"type":"step_finish","part":{"reason":"tool-calls"}}"#,
            "\n",
            r#"{"type":"text","part":{"type":"text","text":"La reponse du modele."}}"#,
            "\n",
            r#"{"type":"step_finish","part":{"reason":"stop"}}"#,
        );
        assert_eq!(
            parse_events(raw),
            Parsed::Text("La reponse du modele.".into())
        );
    }

    #[test]
    fn erreur_amont_remontee() {
        // forme réelle : UnknownError / Unexpected server error
        let raw = r#"{"type":"error","timestamp":1,"error":{"name":"UnknownError","data":{"message":"Unexpected server error. Check server logs for details.","ref":"err_4e25"}}}"#;
        match parse_events(raw) {
            Parsed::Error(e) => {
                assert!(e.contains("UnknownError"), "{e}");
                assert!(e.contains("Unexpected server error"), "{e}");
            }
            other => panic!("attendu Error, obtenu {other:?}"),
        }
    }

    #[test]
    fn lignes_non_json_ignorees() {
        let raw = "bruit opencode\n{\"type\":\"text\",\"part\":{\"text\":\"salut\"}}\n\x1b[0m\n";
        assert_eq!(parse_events(raw), Parsed::Text("salut".into()));
    }

    #[test]
    fn vide_si_aucun_evenement() {
        assert_eq!(parse_events(""), Parsed::Empty);
        assert_eq!(parse_events("rien ici"), Parsed::Empty);
        assert_eq!(parse_events("{ pas du json"), Parsed::Empty);
    }

    #[test]
    fn plusieurs_texts_concatenes() {
        let raw = concat!(
            r#"{"type":"text","part":{"text":"partie 1"}}"#,
            "\n",
            r#"{"type":"text","part":{"text":"partie 2"}}"#,
        );
        assert_eq!(
            parse_events(raw),
            Parsed::Text("partie 1\n\npartie 2".into())
        );
    }

    #[test]
    fn erreur_prime_sur_texte() {
        let raw = concat!(
            r#"{"type":"text","part":{"text":"partial"}}"#,
            "\n",
            r#"{"type":"error","error":{"name":"UsageLimitError","data":{"message":"Free usage limit exceeded"}}}"#,
        );
        match parse_events(raw) {
            Parsed::Error(e) => assert!(e.contains("UsageLimitError"), "{e}"),
            other => panic!("attendu Error, obtenu {other:?}"),
        }
    }
}
