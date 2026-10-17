//! Composition du prompt et validation des noms de modèle.

/// Consigne injectée entre le system prompt du client et la question.
/// Sans outils déclarés par le client : mode TEXTE PUR — opencode ne peut
/// exécuter rien, le modèle ne doit ni annoncer d'action ni attendre un
/// tool_call (symptômes vus en prod Hermes : anglais + coupure sur « : »).
const TEXT_ONLY_RULES: &str = "[Règles du proxy — prioritaires]\n\
- Mode texte pur : AUCUN outil disponible (pas de terminal, pas de lecture \
de fichiers, pas de web, pas d'action). N'écris JAMAIS « je vais \
lire/exécuter/vérifier » ni « I'll ... » : réponds directement et \
COMPLÈTEMENT avec ce que tu sais, ou dis que tu ne peux pas agir.\n\
- Réponds dans la langue du dernier message utilisateur (français si la \
question est en français) — même si les consignes système sont en anglais.\n\
- Garde l'identité et la persona définies par la consigne système.";

/// Avec tools déclarés : le backend opencode exécute RÉELLEMENT via son
/// agent build (bash/read/edit) — il ne faut PAS lui interdire d'agir,
/// ni lui demander de tout résumer : laisse-le travailler et rapporter.
const AGENT_RULES: &str = "[Règles d'exécution — prioritaires]\n\
- Tu as des outils réels (terminal, lecture/écriture de fichiers) : \
UTILISE-LES pour accomplir la tâche, ne te contente pas de décrire ce \
que tu ferais. Termine par un RÉSUMÉ COMPLET du travail fait et des \
résultats obtenus.\n\
- Réponds dans la langue du dernier message utilisateur (français si la \
question est en français).";

/// Consigne selon que le client déclare des tools ou non.
fn proxy_rules(has_tools: bool) -> &'static str {
    if has_tools {
        AGENT_RULES
    } else {
        TEXT_ONLY_RULES
    }
}

/// Aplatit les `messages` OpenAI en un prompt texte unique pour
/// `opencode run` (stdin). Même sémantique que l'ancien pont JS :
/// dernier message user = question, system = consigne préfixée,
/// règles proxy insérées juste avant la question (les LLM suivent mieux
/// les consignes les plus proches de la fin du prompt), et les DERNIERS
/// échanges user/assistant conservés comme contexte (sans quoi une
/// conversation Hermes perd le fil après le premier tour).
pub fn compose_prompt(messages: &serde_json::Value, has_tools: bool) -> Result<String, String> {
    let arr = messages
        .as_array()
        .ok_or_else(|| "messages: tableau attendu".to_string())?;
    let mut sys = String::new();
    // (role, texte) dans l'ordre du fil
    let mut turns: Vec<(String, String)> = Vec::new();
    for m in arr {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let text = match m.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if text.is_empty() {
            continue;
        }
        if role == "system" {
            if !sys.is_empty() {
                sys.push('\n');
            }
            sys.push_str(&text);
        } else if role == "user" || role == "assistant" {
            turns.push((role.to_string(), text));
        }
    }
    // question = DERNIER message user ; le reste = historique
    let q_pos = turns
        .iter()
        .rposition(|(r, _)| r == "user")
        .ok_or_else(|| "aucun contenu user/system dans messages".to_string())?;
    let last_user = turns[q_pos].1.clone();
    let history = &turns[..q_pos];
    if last_user.is_empty() && sys.is_empty() {
        return Err("aucun contenu user/system dans messages".to_string());
    }
    // garder au plus les 6 derniers échanges pour borner le prompt
    let keep = history.len().saturating_sub(6);
    let mut ctx = String::new();
    for (role, text) in &history[keep..] {
        let label = if role == "assistant" {
            "Assistant"
        } else {
            "Utilisateur"
        };
        let t: String = text.chars().take(400).collect();
        ctx.push_str(&format!("{label} : {t}\n"));
    }
    let q = if ctx.is_empty() {
        format!("[Question]\n{last_user}")
    } else {
        format!("[Conversation récente]\n{ctx}\n[Question]\n{last_user}")
    };
    let rules = proxy_rules(has_tools);
    if sys.is_empty() {
        Ok(format!("{rules}\n\n{q}"))
    } else {
        Ok(format!("[Consigne système]\n{sys}\n\n{rules}\n\n{q}"))
    }
}

/// Extrait le répertoire de travail du client depuis les messages system.
/// Hermes le déclare sous des formes variées (« Working directory: X »,
/// « cwd: X », « Current working directory: X »). Retourne None sinon.
/// Sans cwd, l'agent build d'opencode se fait auto-rejeter sur toute
/// lecture hors de SON répertoire (external_directory) et répond vide.
pub fn extract_workdir(messages: &serde_json::Value) -> Option<String> {
    let arr = messages.as_array()?;
    for m in arr {
        if m.get("role").and_then(|v| v.as_str()) != Some("system") {
            continue;
        }
        let text = match m.get("content") {
            Some(serde_json::Value::String(s)) => s.as_str(),
            Some(serde_json::Value::Array(parts)) => {
                // pas de String stable sans allocation ; on cherche direct
                let joined: Vec<&str> = parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect();
                let tmp = joined.join("\n");
                for line in tmp.lines() {
                    if let Some(d) = parse_workdir_line(line) {
                        return Some(d);
                    }
                }
                continue;
            }
            _ => continue,
        };
        for line in text.lines() {
            if let Some(d) = parse_workdir_line(line) {
                return Some(d);
            }
        }
    }
    None
}

fn parse_workdir_line(line: &str) -> Option<String> {
    let l = line.trim();
    let markers = [
        "working directory:",
        "working directory :",
        "répertoire de travail :",
        "repertoire de travail :",
        "cwd:",
        "cwd :",
        "current working directory:",
    ];
    for mk in markers {
        if let Some(rest) = l.to_lowercase().find(mk) {
            let v = l[rest + mk.len()..].trim();
            // élaguer ponctuation de fin (point, guillemets)
            let v = v.trim_end_matches(['.', ',', '"', '\'']);
            if !v.is_empty() && (v.contains('\\') || v.contains('/') || v.contains(':')) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// N'accepte que [a-zA-Z0-9._-] (1..=80 chars) — le nom va dans argv,
/// jamais dans un shell, mais on valide quand même.
pub fn sanitize_model(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= 80
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compose_simple_user() {
        let msgs = json!([{"role": "user", "content": "salut"}]);
        let p = compose_prompt(&msgs, false).unwrap();
        // la question est en clair et le prompt se termine par elle
        assert!(p.contains("[Question]\nsalut"), "p={p}");
        assert!(p.ends_with("salut"));
        // les règles proxy (anti-tool, langue) sont toujours injectées
        assert!(p.contains("Règles du proxy"), "p={p}");
        assert!(p.contains("AUCUN outil"));
    }

    #[test]
    fn compose_system_and_user() {
        let msgs = json!([
            {"role": "system", "content": "tu es concis"},
            {"role": "user", "content": "ping"}
        ]);
        let p = compose_prompt(&msgs, false).unwrap();
        assert!(p.starts_with("[Consigne système]\ntu es concis"), "p={p}");
        assert!(
            p.contains("Règles du proxy"),
            "les règles passent AVANT la question"
        );
        assert!(p.ends_with("[Question]\nping"));
        // ordre : système → règles proxy → question (les consignes les plus
        // proches de la fin gagnent ; le système anglais ne doit pas écraser)
        let i_sys = p.find("[Consigne système]").unwrap();
        let i_rules = p.find("Règles du proxy").unwrap();
        let i_q = p.find("[Question]").unwrap();
        assert!(i_sys < i_rules && i_rules < i_q, "p={p}");
    }

    #[test]
    fn compose_content_parts() {
        let msgs = json!([
            {"role": "user", "content": [{"type": "text", "text": "ligne1"}, {"type": "text", "text": "ligne2"}]}
        ]);
        let p = compose_prompt(&msgs, false).unwrap();
        assert!(p.ends_with("[Question]\nligne1\nligne2"), "p={p}");
    }

    #[test]
    fn compose_garde_l_historique_recent() {
        let msgs = json!([
            {"role": "user", "content": "je m'appelle Marc"},
            {"role": "assistant", "content": "Enchanté Marc !"},
            {"role": "user", "content": "tu te souviens de mon prénom ?"}
        ]);
        let p = compose_prompt(&msgs, false).unwrap();
        // la question est le DERNIER user, pas le premier
        assert!(
            p.ends_with("[Question]\ntu te souviens de mon prénom ?"),
            "p={p}"
        );
        // l'historique est présent, dans l'ordre user→assistant
        assert!(p.contains("Utilisateur : je m'appelle Marc"), "p={p}");
        assert!(p.contains("Assistant : Enchanté Marc !"), "p={p}");
        let i_u = p.find("Utilisateur : je m'appelle").unwrap();
        let i_a = p.find("Assistant : Enchanté").unwrap();
        assert!(i_u < i_a, "ordre du fil préservé");
    }

    #[test]
    fn compose_historique_borne_a_6() {
        let mut msgs = vec![];
        for i in 0..10 {
            msgs.push(json!({"role": "user", "content": format!("u{i}")}));
            msgs.push(json!({"role": "assistant", "content": format!("a{i}")}));
        }
        msgs.push(json!({"role": "user", "content": "final"}));
        let p = compose_prompt(&json!(msgs), false).unwrap();
        // seuls les 6 derniers échanges sont gardés : u0/u1 doivent avoir disparu
        assert!(!p.contains("u0 "), "historique non borné: {p}");
        assert!(p.contains("u9"), "les récents sont là");
        assert!(p.ends_with("[Question]\nfinal"));
    }

    #[test]
    fn compose_empty_refused() {
        let msgs = json!([{"role": "assistant", "content": "moi seul"}]);
        assert!(compose_prompt(&msgs, false).is_err());
        assert!(compose_prompt(&json!("pas un tableau"), false).is_err());
    }

    #[test]
    fn sanitize_ok_and_ko() {
        assert!(sanitize_model("space-bunny-free"));
        assert!(sanitize_model("glm-5.3-flash-free"));
        assert!(!sanitize_model(""));
        assert!(!sanitize_model("a; rm -rf /"));
        assert!(!sanitize_model("a b"));
        assert!(!sanitize_model(&"x".repeat(81)));
    }
}
