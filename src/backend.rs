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

/// Exécute `opencode run --agent <plan|build> --model opencode/<model>`
/// avec le prompt sur stdin.
/// `api_key` = Some(clé) → var OPENCODE_API_KEY passée au spawn ;
/// None → la var est explicitement RETIRÉE (mode anonyme, jamais de mix).
pub fn call(
    model: &str,
    prompt: &str,
    timeout: Duration,
    api_key: Option<&str>,
    use_agent_build: bool,
    workdir: Option<&str>,
) -> Result<BackendOk, BackendError> {
    call_at(
        &opencode_path(),
        model,
        prompt,
        timeout,
        api_key,
        &[],
        use_agent_build,
        workdir,
    )
}

/// Comme [`call`] mais avec un exe imposé (tests E2E : binaire stub).
/// `extra_env` : variables additionnelles posées sur le spawn (le stub
/// de test lit `STUB_FAIL` ; la prod n'en passe aucune).
#[allow(clippy::too_many_arguments)]
pub fn call_at(
    exe: &str,
    model: &str,
    prompt: &str,
    timeout: Duration,
    api_key: Option<&str>,
    extra_env: &[(String, String)],
    use_agent_build: bool,
    workdir: Option<&str>,
) -> Result<BackendOk, BackendError> {
    call_at_inner(
        exe,
        model,
        prompt,
        timeout,
        api_key,
        extra_env,
        use_agent_build,
        workdir,
    )
}

/// PATH complet pour le mode agent (outils réels) : git-bash + système,
/// puis le PATH du process routeur en secours. En mode TEXTE, le PATH
/// minimal (System32) suffit — aucun outil n'est exécuté. Vu en prod :
/// l'agent build exécutait `ls` avec le PATH System32 seul → « command
/// not found » sur chaque outil → cascade brûlée.
fn agent_path() -> String {
    let candidates = [
        r"C:\Program Files\Git\usr\bin",
        r"C:\Program Files\Git\mingw64\bin",
        r"C:\Program Files\Git\cmd",
        r"C:\Windows\System32",
        r"C:\Windows",
    ];
    let mut parts: Vec<String> = candidates
        .iter()
        .filter(|p| std::path::Path::new(p).is_dir())
        .map(|p| p.to_string())
        .collect();
    if let Ok(parent) = std::env::var("PATH") {
        parts.push(parent);
    }
    parts.join(";")
}

#[allow(clippy::too_many_arguments)]
fn call_at_inner(
    exe: &str,
    model: &str,
    prompt: &str,
    timeout: Duration,
    api_key: Option<&str>,
    extra_env: &[(String, String)],
    use_agent_build: bool,
    workdir: Option<&str>,
) -> Result<BackendOk, BackendError> {
    let mut cmd = Command::new(exe);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // Choix d'agent opencode :
    // - `build` (par défaut du CLI) : agent codeur avec outils réels
    //   (bash/read/edit) — utilisé quand le CLIENT déclare des tools :
    //   le backend peut alors exécuter la tâche pour de vrai ;
    // - `plan` : agent texte sans outils — sinon le modèle annonce
    //   « I'll read the file : » et s'arrête en attendant un tool_call
    //   qui ne viendra jamais (symptômes vus en prod Hermes).
    let agent = if use_agent_build { "build" } else { "plan" };
    cmd.args([
        "run",
        "--agent",
        agent,
        "--model",
        &format!("opencode/{model}"),
    ]);
    // Workdir du client (extrait du system prompt) : l'agent build ne
    // lit/écrit QUE dans son cwd — sans ça il se fait auto-rejeter sur
    // external_directory et rend une réponse vide (« permission requested…
    // auto-rejecting », vu en prod).
    if let Some(dir) = workdir {
        if std::path::Path::new(dir).is_dir() {
            cmd.args(["--dir", dir]);
        }
    }
    cmd.env("USERPROFILE", "C:\\Users\\Deamon")
        .env("HOMEDRIVE", "C:")
        .env("HOMEPATH", "\\Users\\Deamon")
        .env(
            "PATH",
            if use_agent_build {
                agent_path()
            } else {
                r"C:\Windows\System32".to_string()
            },
        );
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

    // DRAINER stdout/stderr en threads pendant l'attente : sans ça, une
    // réponse longue remplit le buffer du pipe (quelques Ko), l'enfant
    // BLOQUE en écriture, ne termine jamais — deadlock jusqu'au timeout.
    // (vu en prod : in_flight collé, 19 erreurs, latences 23 s+)
    use std::io::Read;
    use std::sync::mpsc;
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    if let Some(o) = child.stdout.take() {
        std::thread::Builder::new()
            .name("pauvre-drain-stdout".into())
            .spawn(move || {
                let mut s = String::new();
                let _ = Read::read_to_string(&mut { o }, &mut s);
                let _ = tx_out.send(s);
            })
            .ok();
    }
    if let Some(e) = child.stderr.take() {
        std::thread::Builder::new()
            .name("pauvre-drain-stderr".into())
            .spawn(move || {
                let mut s = String::new();
                let _ = Read::read_to_string(&mut { e }, &mut s);
                let _ = tx_err.send(s);
            })
            .ok();
    }

    let deadline = Instant::now() + timeout;
    // attente avec deadline : le drain se fait en parallèle, plus de deadlock
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // les threads de drain terminent naturellement après l'exit
                let out = rx_out
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap_or_default();
                let err = rx_err
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap_or_default();
                let remaining = deadline.saturating_duration_since(Instant::now());
                let elapsed = timeout.saturating_sub(remaining);
                if status.success() {
                    // MODE AGENT : la réponse de l'agent et la transcription
                    // de ses outils sortent sur STDERR (28 Ko vus en prod),
                    // stdout n'a que la bannière ANSI. On traite les deux
                    // comme UNE transcription — sinon on jette la réponse
                    // réelle et on rate le texte au client.
                    let out_clean = strip_ansi(&out);
                    let mut combined = out_clean.trim().to_string();
                    if !err.is_empty() {
                        if !combined.is_empty() {
                            combined.push_str("\n\n");
                        }
                        combined.push_str(strip_ansi(&err).trim());
                    }
                    let text = strip_banner(&combined);
                    if text.is_empty() {
                        // succès SANS texte : diagnostic (bannière seule ?
                        // stdout vide ?) — vu en prod le 21 oct, réponse
                        // vide instantanée servie au client
                        eprintln!(
                            "[pauvre-router] backend success VIDE: out={}b err={}b",
                            out.len(),
                            err.len()
                        );
                        if !err.is_empty() {
                            return Err(BackendError::Failed { raw: err, elapsed });
                        }
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

/// Retire les séquences d'échappement ANSI (couleurs opencode : \x1b[0m
/// etc.) — sinon strip_banner rate la bannière et le texte part avec ses
/// codes de contrôle au client.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // \x1b[ ... lettre finale
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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

    // Les tests d'exécution du backend (call_at contre le binaire stub)
    // vivent dans tests/backend_stub.rs : CARGO_BIN_EXE_* n'est défini
    // que pour les cibles d'intégration, pas pour les tests de la lib.
}
