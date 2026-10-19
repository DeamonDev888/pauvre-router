//! Stub `opencode` pour les tests E2E — n'est JAMAIS livré en prod.
//!
//! Décision de réponse :
//! - env `STUB_FAIL` (liste CSV de modèles, posée par la config serveur
//!   du test) : le modèle courant (`--model`, préfixé `opencode/`) se
//!   termine par l'un d'eux → exit 1 avec une erreur d'exhaustion ;
//! - prompt `SLOW <ms>` : dort puis répond (déclenche le timeout routeur) ;
//! - prompt `ECHOKEY...` : renvoie OPENCODE_API_KEY reçu (ou ANONYMOUS) —
//!   preuve de la rotation de clés ;
//! - sinon : echo `ack <model>: <prompt>` — preuve du chemin complet.

use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let mut prompt = String::new();
    let _ = std::io::stdin().read_to_string(&mut prompt);
    let p = prompt.trim().to_string();
    // le prompt compose contient [Règles du proxy]...[Question] ; le stub
    // ne s'intéresse qu'à la question (compat des assertions E2E)
    let q = p
        .rsplit_once("[Question]\n")
        .map(|(_, q)| q.trim())
        .unwrap_or(&p)
        .to_string();

    if let Some(rest) = q.strip_prefix("SLOW ") {
        let ms: u64 = rest.trim().parse().unwrap_or(50);
        std::thread::sleep(std::time::Duration::from_millis(ms));
        println!("slow done {model}");
        return;
    }

    if q.starts_with("FLAGGED") {
        eprintln!(
            "Error: OpenCode's free models aren't available from your current network because we've detected abusive traffic from this IP address. This isn't related to your account. If you're on a VPN or proxy, turn it off and try again, or subscribe to OpenCode Go at https://opencode.ai/go to keep building."
        );
        std::process::exit(1);
    }

    if let Some(rest) = q.strip_prefix("COOLDOWN ") {
        let secs: u64 = rest.trim().parse().unwrap_or(30);
        eprintln!("Error: rate limit exceeded, retry after {secs} seconds");
        std::process::exit(1);
    }

    let fail_list = std::env::var("STUB_FAIL").unwrap_or_default();
    let should_fail = !fail_list.is_empty()
        && fail_list
            .split(',')
            .any(|m| !m.trim().is_empty() && model.ends_with(m.trim()));

    if should_fail {
        eprintln!(
            "Error: {{\"name\":\"UsageLimitError\",\"data\":{{\"message\":\"Free usage limit exceeded for {model}\"}}}}"
        );
        std::process::exit(1);
    }

    if q.starts_with("ECHOKEY") {
        match std::env::var("OPENCODE_API_KEY") {
            Ok(k) if !k.is_empty() => println!("KEY={k} model={model}"),
            _ => println!("KEY=ANONYMOUS model={model}"),
        }
        return;
    }

    println!("ack {model}: {q}");
}
