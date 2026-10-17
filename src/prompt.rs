//! Composition du prompt et validation des noms de modèle.

/// Aplatit les `messages` OpenAI en un prompt texte unique pour
/// `opencode run` (stdin). Même sémantique que l'ancien pont JS :
/// dernier message user = question, system = consigne préfixée.
pub fn compose_prompt(messages: &serde_json::Value) -> Result<String, String> {
    let arr = messages
        .as_array()
        .ok_or_else(|| "messages: tableau attendu".to_string())?;
    let mut sys = String::new();
    let mut last_user = String::new();
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
        } else if role == "user" {
            last_user = text;
        }
    }
    if last_user.is_empty() && sys.is_empty() {
        return Err("aucun contenu user/system dans messages".to_string());
    }
    if sys.is_empty() {
        Ok(last_user)
    } else {
        Ok(format!(
            "[Consigne système]\n{sys}\n\n[Question]\n{last_user}"
        ))
    }
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
        assert_eq!(compose_prompt(&msgs).unwrap(), "salut");
    }

    #[test]
    fn compose_system_and_user() {
        let msgs = json!([
            {"role": "system", "content": "tu es concis"},
            {"role": "user", "content": "ping"}
        ]);
        assert_eq!(
            compose_prompt(&msgs).unwrap(),
            "[Consigne système]\ntu es concis\n\n[Question]\nping"
        );
    }

    #[test]
    fn compose_content_parts() {
        let msgs = json!([
            {"role": "user", "content": [{"type": "text", "text": "ligne1"}, {"type": "text", "text": "ligne2"}]}
        ]);
        assert_eq!(compose_prompt(&msgs).unwrap(), "ligne1\nligne2");
    }

    #[test]
    fn compose_empty_refused() {
        let msgs = json!([{"role": "assistant", "content": "moi seul"}]);
        assert!(compose_prompt(&msgs).is_err());
        assert!(compose_prompt(&json!("pas un tableau")).is_err());
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
