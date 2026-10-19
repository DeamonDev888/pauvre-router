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
            ErrorKind::ModelInvalid
        );
        assert_eq!(classify("unknown model: toto"), ErrorKind::ModelInvalid);
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
