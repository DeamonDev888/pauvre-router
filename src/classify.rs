//! Classification des erreurs backend : exhaustion (quota/limite),
//! auth, wrong-model, ou erreur générique. Base des cooldowns et du
//! déclenchement de la cascade.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Quota épuisé / rate-limit / "exhausted" / 402-429-ish.
    Exhausted,
    /// Modèle inconnu / non accessible sur ce compte.
    ModelInvalid,
    /// IP flaggée « abusive traffic » par l'amont (anti-ban).
    IpFlagged,
    /// Erreur générique ( réseau, server error, spawn ).
    Transient,
}

impl ErrorKind {
    /// Label court pour les messages clients et les logs.
    pub fn label(&self) -> &'static str {
        match self {
            ErrorKind::Exhausted => "Exhausted",
            ErrorKind::ModelInvalid => "ModelInvalid",
            ErrorKind::IpFlagged => "IpFlagged",
            ErrorKind::Transient => "Transient",
        }
    }
}

/// Classifie la sortie brute (stderr/stdout combinés) du CLI.
pub fn classify(raw: &str) -> ErrorKind {
    if is_ip_flagged(raw) {
        return ErrorKind::IpFlagged;
    }
    let l = raw.to_ascii_lowercase();
    let exhausted = [
        "exhausted",
        "quota",
        "usage limit",
        "usage_limit",
        "rate limit",
        "rate_limit",
        "too many requests",
        "insufficient",
        "out of credits",
        "no credits",
        "billing",
        "payment",
        "402",
        "429",
    ];
    if exhausted.iter().any(|k| l.contains(k)) {
        return ErrorKind::Exhausted;
    }
    // "Unexpected server error" = erreur GÉNÉRIQUE de la gateway amont
    // (panne serveur chez opencode), PAS un modèle inconnu. Elle doit
    // être distinguée AVANT le bucket "invalid" : le message contient
    // « not available » dans certaines variantes → sinon on clouait un
    // modèle VALIDE pendant 1 h (COOLDOWN_MODEL_INVALID) sur un simple
    // hiccup de la gateway. Symptôme vu en prod : minimax-m3.1-flash-free
    // et longcat-2.5-preview-free « UnknownError / Unexpected server
    // error » → cd=3600 s, alors que ces modèles répondent par ailleurs.
    if l.contains("unexpected server error") {
        return ErrorKind::Transient;
    }
    let invalid = [
        "unknown model",
        "unknownerror",
        "model not found",
        "not available",
        "does not exist",
        "invalid model",
        "unsupported model",
        "1211",
        "not entitled",
        "no access",
    ];
    if invalid.iter().any(|k| l.contains(k)) {
        return ErrorKind::ModelInvalid;
    }
    ErrorKind::Transient
}

/// Bannière de flag IP de l'amont (incident du 18 oct 2026 : les deux IPs
/// du PC — VPN puis maison — flaggées « abusive traffic » pendant ~24 h).
/// Détectée tôt dans [`classify`] : le message amont contient « not
/// available » qui otherwise tomberait dans le bucket ModelInvalid.
pub fn is_ip_flagged(raw: &str) -> bool {
    let l = raw.to_ascii_lowercase();
    l.contains("abusive traffic")
        || (l.contains("free models aren't available") && l.contains("from your current network"))
}

/// Extrait un « réessaie dans N secondes » de la sortie amont (rate limit,
/// cooldown explicite). Retourne None si rien n'est trouvé.
pub fn cooldown_secs(raw: &str) -> Option<u64> {
    let l = raw.to_ascii_lowercase();
    for marker in [
        "retry after",
        "retry_after",
        "réessaie dans",
        "reessaie dans",
    ] {
        if let Some(pos) = l.find(marker) {
            let rest = &l[pos + marker.len()..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(v) = digits.parse::<u64>() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustion_detectee() {
        assert_eq!(
            classify("Error: {\"name\":\"UsageLimitError\",\"data\":{\"message\":\"Free usage limit exceeded\"}}"),
            ErrorKind::Exhausted
        );
        assert_eq!(
            classify("Too many requests, retry later"),
            ErrorKind::Exhausted
        );
        assert_eq!(
            classify("429 quota exceeded for free tier"),
            ErrorKind::Exhausted
        );
    }

    #[test]
    fn model_invalide_detecte() {
        assert_eq!(
            classify("Error: {\"name\":\"UnknownError\",\"data\":{\"message\":\"Unexpected server error.\"}}"),
            ErrorKind::Transient
        );
        assert_eq!(classify("unknown model: toto"), ErrorKind::ModelInvalid);
    }

    /// RÉGRESSION v0.9.0 : « Unexpected server error » est une panne
    /// GÉNÉRIQUE de la gateway, PAS un modèle inconnu. Le message
    /// contient « not available » dans certaines variantes → sans test
    /// explicite AVANT le bucket « invalid », un modèle VALIDE était
    /// cloué `COOLDOWN_MODEL_INVALID` (3600 s) sur un hiccup.
    #[test]
    fn unexpected_server_error_est_transient_pas_model_invalide() {
        let raw = "UnknownError: Unexpected server error. Check server logs for details.";
        assert_eq!(classify(raw), ErrorKind::Transient);
        // variante qui contient « not available » : ne doit PAS basculer
        // en ModelInvalid
        let variante = "UnknownError: Unexpected server error. Model not available right now.";
        assert_eq!(classify(variante), ErrorKind::Transient);
        // et un VRAI modèle inconnu reste ModelInvalid
        assert_eq!(classify("unknown model: toto"), ErrorKind::ModelInvalid);
        assert_eq!(
            classify("model not available on this account"),
            ErrorKind::ModelInvalid
        );
    }

    /// L'ordre des buckets est structurant : « exhausted » doit gagner
    /// contre « invalid », sinon un quota épuisé cloue un modèle 1 h
    /// au lieu de 2 min.
    #[test]
    fn ordre_des_buckets_exhausted_puis_invalid() {
        // contient 429 ET "unknown model" → Exhausted (quota d'abord)
        assert_eq!(
            classify("429 unknown model quota exceeded"),
            ErrorKind::Exhausted
        );
        // le flag IP bat TOUT (testé avant l'autre bucket)
        let flagged = "OpenCode's free models aren't available from your current network because we've detected abusive traffic from this IP address.";
        assert_eq!(classify(flagged), ErrorKind::IpFlagged);
    }

    /// La forme d'erreur STRUCTURÉE réellement émise par opencode
    /// (`--format json`) doit être classée : c'est ce chemin qui
    /// remplace le transcript brut de 28 Ko.
    #[test]
    fn erreur_structuree_opencode_classee() {
        let raw = "UnknownError: Unexpected server error. Check server logs for details.";
        assert_eq!(classify(raw), ErrorKind::Transient);
        let quota = "UsageLimitError: Free usage limit exceeded for glm-5.3-flash-free";
        assert_eq!(classify(quota), ErrorKind::Exhausted);
        let auth = "ProviderAuthError: free models aren't available from your current network because we've detected abusive traffic from this IP address.";
        assert_eq!(classify(auth), ErrorKind::IpFlagged);
    }

    #[test]
    fn transient_par_defaut() {
        assert_eq!(classify("network unreachable"), ErrorKind::Transient);
        assert_eq!(classify(""), ErrorKind::Transient);
        assert_eq!(
            classify("spawn failed: file not found"),
            ErrorKind::Transient
        );
    }

    #[test]
    fn ban_ip_detecte_avant_tout() {
        // message réel de l'incident du 18 oct 2026 (sortie CLI)
        let raw = "Error: OpenCode's free models aren't available from your current network because we've detected abusive traffic from this IP address. This isn't related to your account.";
        assert_eq!(classify(raw), ErrorKind::IpFlagged);
        assert!(is_ip_flagged(raw));
        // sans « abusive traffic », la 2e empreinte suffit
        assert!(is_ip_flagged(
            "free models aren't available from your current network, sorry"
        ));
        // « not available » seul reste ModelInvalid (pas un flag IP)
        assert_eq!(
            classify("model not available on this account"),
            ErrorKind::ModelInvalid
        );
        assert!(!is_ip_flagged("model not available on this account"));
        assert_eq!(ErrorKind::IpFlagged.label(), "IpFlagged");
    }

    #[test]
    fn cooldown_secs_extrait_le_delai() {
        assert_eq!(
            cooldown_secs("Rate limit exceeded. Retry after 45 seconds"),
            Some(45)
        );
        assert_eq!(cooldown_secs("error: {\"retry_after\":30}"), Some(30));
        assert_eq!(cooldown_secs("veillez réessaie dans 12 s"), Some(12));
        assert_eq!(cooldown_secs("rien ici"), None);
        assert_eq!(cooldown_secs("retry after bientôt"), None);
    }
}
