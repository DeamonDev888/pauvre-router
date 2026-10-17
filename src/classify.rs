//! Classification des erreurs backend : exhaustion (quota/limite),
//! auth, wrong-model, ou erreur générique. Base des cooldowns et du
//! déclenchement de la cascade.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Quota épuisé / rate-limit / "exhausted" / 402-429-ish.
    Exhausted,
    /// Modèle inconnu / non accessible sur ce compte.
    ModelInvalid,
    /// Erreur générique ( réseau, server error, spawn ).
    Transient,
}

/// Classifie la sortie brute (stderr/stdout combinés) du CLI.
pub fn classify(raw: &str) -> ErrorKind {
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
}
