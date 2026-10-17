//! Backend : spawn direct de `opencode.exe run` (le client officiel passe
//! la gate fingerprint Zen). stdin = prompt, stdout = texte de réponse.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Chemin par défaut de l'exe opencode (override via env PAUVRE_OPENCODE).
pub const DEFAULT_OPENCODE: &str = r"C:\Users\Deamon\AppData\Local\hermes\tools\node-26.7.0-win32-x64\node_modules\opencode-ai\bin\opencode.exe";

/// Résultat d'un appel backend.
#[derive(Debug, Clone)]
pub struct BackendOk {
    pub text: String,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum BackendError {
    /// Le CLI a répondu une erreur (contient la sortie combinée, classée ensuite).
    Failed { raw: String, elapsed: Duration },
    /// Timeout / crash sans sortie exploitable.
    Timeout { elapsed: Duration },
}

impl BackendError {
    pub fn elapsed(&self) -> Duration {
        match self {
            BackendError::Failed { elapsed, .. } | BackendError::Timeout { elapsed } => *elapsed,
        }
    }
}

/// Chemin opencode résolu (env > défaut).
pub fn opencode_path() -> String {
    std::env::var("PAUVRE_OPENCODE").unwrap_or_else(|_| DEFAULT_OPENCODE.to_string())
}

/// Exécute `opencode run --model opencode/<model>` avec le prompt sur stdin.
/// `api_key` = Some(clé) → var OPENCODE_API_KEY passée au spawn ;
/// None → la var est explicitement RETIRÉE (mode anonyme, jamais de mix).
pub fn call(
    model: &str,
    prompt: &str,
    timeout: Duration,
    api_key: Option<&str>,
) -> Result<BackendOk, BackendError> {
    let exe = opencode_path();
    let mut cmd = Command::new(&exe);
    cmd.args(["run", "--model", &format!("opencode/{model}")])
        .env("USERPROFILE", "C:\\Users\\Deamon")
        .env("HOMEDRIVE", "C:")
        .env("HOMEPATH", "\\Users\\Deamon")
        .env("PATH", r"C:\Windows\System32");
    match api_key {
        Some(k) => cmd.env("OPENCODE_API_KEY", k),
        None => cmd.env_remove("OPENCODE_API_KEY"),
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| BackendError::Failed {
            raw: format!("spawn failed: {e}"),
            elapsed: Duration::ZERO,
        })?;

    // écrire le prompt puis fermer stdin (EOF => le CLI lit tout)
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(prompt.as_bytes());
        let _ = si.flush();
        drop(si);
    }

    let deadline = Instant::now() + timeout;
    // lecture bloquante avec veille de dépassement : wait + check périodique
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut o) = child.stdout.take() {
                    use std::io::Read;
                    let _ = o.read_to_string(&mut out);
                }
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                let elapsed = timeout.saturating_sub(deadline - Instant::now());
                if status.success() {
                    let text = strip_banner(&out);
                    if text.is_empty() && !err.is_empty() {
                        return Err(BackendError::Failed { raw: err, elapsed });
                    }
                    return Ok(BackendOk { text, elapsed });
                }
                let mut raw = err;
                if raw.is_empty() {
                    raw = out;
                }
                return Err(BackendError::Failed { raw, elapsed });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(BackendError::Timeout { elapsed: timeout });
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            Err(e) => {
                return Err(BackendError::Failed {
                    raw: format!("wait failed: {e}"),
                    elapsed: Duration::ZERO,
                })
            }
        }
    }
}

/// Retire la bannière `> build · model` (première ligne) + trim.
pub fn strip_banner(out: &str) -> String {
    let mut s = out.trim().to_string();
    if let Some(rest) = s.strip_prefix('>') {
        if let Some(nl) = rest.find('\n') {
            s = rest[nl + 1..].trim().to_string();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_retiree() {
        assert_eq!(strip_banner("> build · space-bunny-free\npong"), "pong");
        assert_eq!(strip_banner("pas de bannière"), "pas de bannière");
        assert_eq!(strip_banner("> build\n\nmulti\nlignes"), "multi\nlignes");
        assert_eq!(strip_banner("  "), "");
    }

    #[test]
    fn opencode_path_env_override() {
        std::env::set_var("PAUVRE_OPENCODE", "X:\\fake\\opencode.exe");
        assert_eq!(opencode_path(), "X:\\fake\\opencode.exe");
        std::env::remove_var("PAUVRE_OPENCODE");
        assert_eq!(opencode_path(), DEFAULT_OPENCODE);
    }
}
