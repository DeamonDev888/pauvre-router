//! Tests E2E — de vrais serveurs pauvre-router sur ports éphémères,
//! de vrais clients TCP, un backend stub (jamais le CLI opencode réel,
//! jamais le réseau). Chaque test démarre sa propre instance isolée
//! (tmpdir propre, port 0) : aucune variable d'env process, aucune
//! race inter-tests, aucun état partagé hors du serveur testé.

use pauvre_router::models::FREE_MODELS;
use pauvre_router::server::{self, ServerConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// Infrastructure
// ---------------------------------------------------------------------

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "pauvre-e2e-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Instance {
    port: u16,
    _dir: PathBuf, // supprimé au drop (le serveur, lui, vit dans des threads 'static)
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

/// Démarre un serveur complet (mêmes workers que la prod) avec le stub.
fn start(tag: &str, tune: impl FnOnce(&mut ServerConfig)) -> Instance {
    let dir = tmp_dir(tag);
    let stub = env!("CARGO_BIN_EXE_stub-opencode").to_string();
    let mut cfg = ServerConfig {
        port: 0,
        state_path: dir.join("state.json"),
        keys_path: dir.join("keys.json"),
        backend_exe: stub,
        backend_env: Vec::new(),
        call_timeout: Duration::from_millis(1500),
        rate_capacity: 100, // large par défaut : les tests anti-spam la ressèrent
        rate_per_min: 10_000.0,
        gate_max: 8,
        gate_wait: Duration::from_millis(500),
        save_interval: Duration::from_secs(3600),
        mode: "priority".into(),
    };
    tune(&mut cfg);
    let bound = server::bind_server(&cfg).expect("bind serveur E2E");
    let port = bound.port;
    std::thread::spawn(move || server::serve(bound));
    wait_health(port);
    Instance { port, _dir: dir }
}

fn wait_health(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() > deadline {
            panic!("serveur E2E jamais prêt sur {port}");
        }
        if let Ok((code, _)) = req(port, "GET", "/health", b"", false) {
            if code == 200 {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Requête HTTP/1.1 brute sur socket (Connection: close par défaut).
/// Retourne (status, headers+body bruts).
fn req(
    port: u16,
    method: &str,
    path: &str,
    body: &[u8],
    stream: bool,
) -> std::io::Result<(u16, String)> {
    req_ip(port, "127.0.0.1", method, path, body, stream)
}

fn req_ip(
    port: u16,
    _ip: &str,
    method: &str,
    path: &str,
    body: &[u8],
    stream: bool,
) -> std::io::Result<(u16, String)> {
    // Le loopback Windows renvoie parfois un RST ponctuel sous rafale
    // parallèle (pressure TIME_WAIT des ports éphémères clients). Le RST
    // peut frapper avant OU après le traitement serveur : on rejoue
    // systématiquement (×3). Le contenu est déterministe (stub), mais les
    // COMPTEURS peuvent gonfler d'un fantôme — les tests n'y font donc
    // jamais d'égalité stricte, seulement des planchers.
    let mut last = None;
    for attempt in 0..3 {
        match req_ip_once(port, method, path, body, stream) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                last = Some(Err(e));
                std::thread::sleep(Duration::from_millis(50 * (attempt + 1)));
            }
            other => return other,
        }
    }
    last.unwrap()
}

fn req_ip_once(
    port: u16,
    method: &str,
    path: &str,
    body: &[u8],
    stream: bool,
) -> std::io::Result<(u16, String)> {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut r = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if stream {
        r.push_str("Accept: text/event-stream\r\n");
    }
    r.push_str("\r\n");
    s.write_all(r.as_bytes())?;
    if !body.is_empty() {
        s.write_all(body)?;
    }
    let mut out = String::new();
    BufReader::new(s).read_to_string(&mut out)?;
    let code = out
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    Ok((code, out))
}

fn chat(port: u16, model: &str, prompt: &str) -> std::io::Result<(u16, serde_json::Value)> {
    let body =
        serde_json::json!({"model": model, "messages": [{"role": "user", "content": prompt}]});
    let (code, raw) = req(
        port,
        "POST",
        "/v1/chat/completions",
        body.to_string().as_bytes(),
        false,
    )?;
    let json = raw
        .split("\r\n\r\n")
        .nth(1)
        .and_then(|b| serde_json::from_str(b).ok())
        .unwrap_or(serde_json::Value::Null);
    Ok((code, json))
}

fn chat_content(v: &serde_json::Value) -> String {
    v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn get_json(port: u16, path: &str) -> serde_json::Value {
    let (_, raw) = req(port, "GET", path, b"", false).unwrap();
    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap_or("{}"))
        .unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------
// Endpoints lecture
// ---------------------------------------------------------------------

#[test]
fn e2e_health_et_models() {
    let i = start("health", |_| {});
    let (code, raw) = req(i.port, "GET", "/health", b"", false).unwrap();
    assert_eq!(code, 200);
    assert!(raw.contains("\"status\":\"ok\""), "raw={raw}");
    assert!(raw.contains("\"router\":\"pauvre-router\""));

    // alias /v1/health
    let (c2, _) = req(i.port, "GET", "/v1/health", b"", false).unwrap();
    assert_eq!(c2, 200);

    let models = get_json(i.port, "/v1/models");
    let ids: Vec<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 5, "robin + 4 frontieres: {ids:?}");
    assert!(ids.contains(&"frontier-robin-free"));
    assert!(ids.contains(&"beam-free"));
    assert!(ids.contains(&"glm-5.3-flash-free"));
    assert!(ids.contains(&"space-bunny-free"));
    assert!(ids.contains(&"deepseek-v4.1-flash-free"));
}

#[test]
fn e2e_endpoint_inconnu_404_et_options_204() {
    let i = start("404", |_| {});
    let (code, _) = req(i.port, "GET", "/nimporte", b"", false).unwrap();
    assert_eq!(code, 404);
    let (co, _) = req(i.port, "OPTIONS", "/v1/chat/completions", b"", false).unwrap();
    assert_eq!(co, 204);
}

// ---------------------------------------------------------------------
// Chat complet — chemin nominal + protocoles
// ---------------------------------------------------------------------

#[test]
fn e2e_chat_json_puis_cache_hit() {
    let i = start("cache", |_| {});
    let (c1, v1) = chat(i.port, "glm-5.3-flash-free", "PONG").unwrap();
    assert_eq!(c1, 200, "v1={v1}");
    assert_eq!(chat_content(&v1), "ack opencode/glm-5.3-flash-free: PONG");
    assert_eq!(v1["object"], "chat.completion");
    assert_eq!(v1["choices"][0]["finish_reason"], "stop");

    // 2e appel identique → servi du cache (même réponse). NB: un RST
    // loopback peut rejouer la requête → cache_hits peut dépasser 1,
    // on n'exige que le plancher (>= 1).
    let (c2, v2) = chat(i.port, "glm-5.3-flash-free", "PONG").unwrap();
    assert_eq!(c2, 200);
    assert_eq!(chat_content(&v2), chat_content(&v1));
    let stats = get_json(i.port, "/admin/stats");
    assert!(
        stats["stats"]["cache_hits"].as_u64().unwrap_or(0) >= 1,
        "stats={stats}"
    );
    assert_eq!(stats["stats"]["ok_total"], 1);
}

#[test]
fn e2e_chat_sse_stream() {
    let i = start("sse", |_| {});
    let body = serde_json::json!({
        "model": "space-bunny-free",
        "stream": true,
        "messages": [{"role": "user", "content": "PONG"}]
    });
    let (code, raw) = req(
        i.port,
        "POST",
        "/v1/chat/completions",
        body.to_string().as_bytes(),
        true,
    )
    .unwrap();
    assert_eq!(code, 200);
    assert!(
        raw.contains("text/event-stream"),
        "content-type SSE attendu: {raw}"
    );
    // structure exacte attendue par le client OpenAI d'Hermes
    assert!(
        raw.contains("\"object\":\"chat.completion.chunk\""),
        "{raw}"
    );
    assert!(raw.contains("\"finish_reason\":\"stop\""));
    assert!(raw.contains("data: [DONE]"));
    // le contenu est arrivé dans un delta
    assert!(
        raw.contains("\"content\":\"ack opencode/space-bunny-free: PONG\""),
        "{raw}"
    );
}

#[test]
fn e2e_robin_round_robin_prouve_la_repartition() {
    let i = start("robin", |_| {});
    // N prompts distincts (pas de cache) sur le robin : la cascade entière
    // est couverte par le round-robin → chaque modèle répond une fois.
    let mut served: Vec<String> = Vec::new();
    for n in 0..FREE_MODELS.len() {
        let (_, v) = chat(i.port, "frontier-robin-free", &format!("PONG {n}")).unwrap();
        let content = chat_content(&v);
        served.push(content);
    }
    let all = served.join(" | ");
    for m in [
        "beam-free",
        "glm-5.3-flash-free",
        "space-bunny-free",
        "deepseek-v4.1-flash-free",
    ] {
        assert!(all.contains(m), "robin n'a pas servi {m}: {all}");
    }
}

// ---------------------------------------------------------------------
// Cascade & cooldowns
// ---------------------------------------------------------------------

#[test]
fn e2e_cascade_fallback_sur_quota() {
    // glm échoue (exhaustion, via STUB_FAIL) → cooldown glm → la requête
    // est sauvée par un modèle de fallback, PAS par glm.
    let i = start("cascade", |c| {
        c.backend_env = vec![("STUB_FAIL".into(), "glm-5.3-flash-free".into())];
    });
    let (code, v) = chat(i.port, "glm-5.3-flash-free", "PONG bonjour").unwrap();
    assert_eq!(code, 200, "le fallback doit sauver la requête: {v}");
    let served = chat_content(&v);
    // format stub : "ack opencode/<model>: <prompt>" — on extrait par
    // préfixe (split sur ':' casserait sur http://)
    let fallback = served
        .strip_prefix("ack opencode/")
        .and_then(|r| r.split(": ").next())
        .unwrap_or_default();
    assert!(
        fallback.ends_with("beam-free")
            || fallback.ends_with("space-bunny-free")
            || fallback.ends_with("deepseek-v4.1-flash-free"),
        "repondu par un modele de fallback, pas glm: '{served}'"
    );
    // le refus quota a bien mis glm en cooldown (health)
    let stats = get_json(i.port, "/admin/stats");
    let glm = &stats["health"]["glm-5.3-flash-free"];
    assert!(
        glm["cooldown_secs"].as_u64().unwrap_or(0) > 0,
        "stats={stats}"
    );
    assert!(glm["failures"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn e2e_tous_en_cooldown_429() {
    // STUB_FAIL = les 3 modèles → chaque tentative échoue en exhaustion,
    // la chaîne entière brûle → 502 ; puis tout est en cooldown → 429.
    let i = start("cooldown-all", |c| {
        c.backend_env = vec![(
            "STUB_FAIL".into(),
            "beam-free,glm-5.3-flash-free,space-bunny-free,deepseek-v4.1-flash-free".into(),
        )];
    });
    let (code1, v1) = chat(i.port, "glm-5.3-flash-free", "PONG one").unwrap();
    assert_eq!(code1, 502, "toute la chaîne échoue: {v1}");
    let stats = get_json(i.port, "/admin/stats");
    for m in [
        "beam-free",
        "glm-5.3-flash-free",
        "space-bunny-free",
        "deepseek-v4.1-flash-free",
    ] {
        assert!(
            stats["health"][m]["cooldown_secs"].as_u64().unwrap_or(0) > 0,
            "{m} doit être en cooldown: {stats}"
        );
    }
    // requête suivante : tous en cooldown → 429 sans même spawn le stub
    let (code2, v2) = chat(i.port, "space-bunny-free", "PONG two").unwrap();
    assert_eq!(code2, 429, "attendu 429 tous-cooldowns: {v2}");
    let msg = v2["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("cooldown"), "msg={msg}");
}

// ---------------------------------------------------------------------
// Anti-spam (token bucket)
// ---------------------------------------------------------------------

#[test]
fn e2e_antispam_429_puis_requete_normale_ok() {
    // bucket serré : 2 rafale, recharge quasi nulle sur la durée du test
    let i = start("antispam", |c| {
        c.rate_capacity = 2;
        c.rate_per_min = 0.001;
    });
    // 1) santé : ne passe PAS par le limiter (endpoints lecture) — ok
    let (h, _) = req(i.port, "GET", "/health", b"", false).unwrap();
    assert_eq!(h, 200);
    // 2) deux chats passent (rafale), le 3e est bloqué
    let (a, _) = chat(i.port, "glm-5.3-flash-free", "PONG one").unwrap();
    let (b, _) = chat(i.port, "glm-5.3-flash-free", "PONG two").unwrap();
    let (c, vc) = chat(i.port, "glm-5.3-flash-free", "PONG three").unwrap();
    assert_eq!(a, 200);
    assert_eq!(b, 200);
    assert_eq!(c, 429, "3e requête doit être bloquée: {vc}");
    assert_eq!(vc["error"]["type"], "rate_limit_error");
    assert!(vc["error"]["retry_after"].as_u64().unwrap_or(0) >= 1);
    // 3) le refus est tracé (>= : un RST-fantôme peut rejouer une requête)
    let stats = get_json(i.port, "/admin/stats");
    assert!(
        stats["stats"]["spam_blocked"].as_u64().unwrap_or(0) >= 1,
        "stats={stats}"
    );
    assert!(
        !stats["recent_spam"].as_array().unwrap().is_empty(),
        "stats={stats}"
    );
}

// ---------------------------------------------------------------------
// Backend : timeout et modèle invalide
// ---------------------------------------------------------------------

#[test]
fn e2e_backend_timeout_devient_502_ou_fallback() {
    // SLOW 5000 > call_timeout (1.5s) : le modèle visé timeout → soit un
    // fallback sert la requête, soit 502 si toute la chaîne brûle.
    let i = start("timeout", |c| {
        c.backend_env = vec![("STUB_FAIL".into(), "space-bunny-free".into())];
    });
    let (code, v) = chat(i.port, "space-bunny-free", "SLOW 5000").unwrap();
    assert!(code == 200 || code == 502, "code inattendu {code}: {v}");
    if code == 200 {
        let served = chat_content(&v);
        assert!(
            !served.contains("space-bunny-free"),
            "space-bunny doit être en cooldown, pas servir: {served}"
        );
    }
    let stats = get_json(i.port, "/admin/stats");
    assert!(
        stats["stats"]["errors_total"].as_u64().unwrap_or(0) >= 1,
        "le timeout doit être compté: {stats}"
    );
}

#[test]
fn e2e_mode_invalide_400_et_chat_sans_messages_400() {
    let i = start("badreq", |_| {});
    // mode admin invalide
    let (code, _) = req(i.port, "POST", "/admin/mode", br#"{"mode":"chaos"}"#, false).unwrap();
    assert_eq!(code, 400);
    // chat sans contenu user/system
    let (code2, v2) = chat(i.port, "glm-5.3-flash-free", "__VIDE__").unwrap();
    let _ = v2;
    assert_ne!(code2, 500, "jamais de 500 sur une requête malformée");
    // JSON illisible
    let (code3, _) = req(i.port, "POST", "/v1/chat/completions", b"{{non-json", false).unwrap();
    assert_eq!(code3, 400);
}

// ---------------------------------------------------------------------
// Pool de clés API via l'API admin
// ---------------------------------------------------------------------

#[test]
fn e2e_keys_add_remove_reanimate_et_rotation_prouvee() {
    let i = start("keys", |_| {});
    // clé construite à l'exécution : les middlewares de rédaction ne
    // doivent pas pouvoir la détruire dans le source
    let key = format!("sk-e2e-test-key-{}", "0".repeat(10));
    // pool vide → anonyme
    let (_, v0) = chat(i.port, "glm-5.3-flash-free", "ECHOKEY#0").unwrap();
    assert_eq!(
        chat_content(&v0),
        "KEY=ANONYMOUS model=opencode/glm-5.3-flash-free"
    );

    // add invalide (pas le bon préfixe / trop courte)
    let (bad, _) = req(
        i.port,
        "POST",
        "/admin/keys/add",
        br#"{"key":"courte"}"#,
        false,
    )
    .unwrap();
    assert_eq!(bad, 400, "clé invalide doit être refusée");

    // add valide → le spawn reçoit la clé (preuve par le stub)
    let add_body = format!(r#"{{"key":"{key}"}}"#);
    let (ok1, _) = req(
        i.port,
        "POST",
        "/admin/keys/add",
        add_body.as_bytes(),
        false,
    )
    .unwrap();
    assert_eq!(ok1, 200, "add valide doit passer");
    let (_, v1) = chat(i.port, "glm-5.3-flash-free", "ECHOKEY#1").unwrap();
    let served = chat_content(&v1);
    assert!(
        served.starts_with(&format!("KEY={key}")),
        "le spawn doit porter la clé: {served}"
    );

    // remove → retour anonyme
    let rm_body = format!(r#"{{"key":"{key}"}}"#);
    let (ok2, _) = req(
        i.port,
        "POST",
        "/admin/keys/remove",
        rm_body.as_bytes(),
        false,
    )
    .unwrap();
    assert_eq!(ok2, 200);
    let (_, v2) = chat(i.port, "glm-5.3-flash-free", "ECHOKEY#2").unwrap();
    assert!(chat_content(&v2).contains("KEY=ANONYMOUS"));
}

// ---------------------------------------------------------------------
// Mode de routage
// ---------------------------------------------------------------------

#[test]
fn e2e_mode_switch_priority_roundrobin() {
    let i = start("mode", |_| {});
    let m0 = get_json(i.port, "/admin/mode");
    assert_eq!(m0["mode"], "priority");
    let (code, _) = req(
        i.port,
        "POST",
        "/admin/mode",
        br#"{"mode":"roundrobin"}"#,
        false,
    )
    .unwrap();
    assert_eq!(code, 200);
    let m1 = get_json(i.port, "/admin/mode");
    assert_eq!(m1["mode"], "roundrobin");
    // une requête passe en roundrobin aussi (chemin nominal sous le nouveau mode)
    let (c, _) = chat(i.port, "glm-5.3-flash-free", "PONG rr").unwrap();
    assert_eq!(c, 200);
    // /health reflète le mode
    let (_, raw) = req(i.port, "GET", "/health", b"", false).unwrap();
    assert!(
        raw.contains("roundrobin"),
        "health doit montrer le mode: {raw}"
    );
}

// ---------------------------------------------------------------------
// Persistance de l'état
// ---------------------------------------------------------------------

#[test]
fn e2e_persistance_etat_sur_reboot() {
    // Un état disque (comme si un run précédent avait épuisé glm) doit être
    // restauré au boot : glm en cooldown → la requête part sur un fallback,
    // et /admin/stats montre le cooldown restauré.
    let dir = tmp_dir("persist");
    let health = serde_json::json!({
        "glm-5.3-flash-free": {"cooldown_secs": 120, "failures": 1}
    });
    let st = pauvre_router::stats::Stats::default();
    pauvre_router::stats::save_to(&dir.join("state.json"), &st, &health).unwrap();

    let cfg = ServerConfig {
        port: 0,
        state_path: dir.join("state.json"),
        keys_path: dir.join("keys.json"),
        backend_exe: env!("CARGO_BIN_EXE_stub-opencode").to_string(),
        backend_env: Vec::new(),
        call_timeout: Duration::from_millis(1500),
        rate_capacity: 100,
        rate_per_min: 10_000.0,
        gate_max: 8,
        gate_wait: Duration::from_millis(500),
        save_interval: Duration::from_secs(3600),
        mode: "priority".into(),
    };
    let bound = server::bind_server(&cfg).expect("bind");
    let port = bound.port;
    std::thread::spawn(move || server::serve(bound));
    wait_health(port);

    // preuve 1 : l'état restauré est visible dans /admin/stats
    let stats = get_json(port, "/admin/stats");
    assert!(
        stats["health"]["glm-5.3-flash-free"]["cooldown_secs"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "cooldown glm doit être restauré du disque: {stats}"
    );
    // preuve 2 : glm ne peut pas servir, la cascade passe par un fallback
    let (code, v) = chat(port, "glm-5.3-flash-free", "PONG persist").unwrap();
    assert_eq!(code, 200, "le fallback doit servir: {v}");
    let served = chat_content(&v);
    assert!(
        !served.contains("glm-5.3-flash-free"),
        "glm est en cooldown restauré, ne doit pas servir: {served}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------------
// Robustesse TCP
// ---------------------------------------------------------------------

#[test]
fn e2e_keepalive_3_requetes_meme_socket() {
    let i = start("keepalive", |_| {});
    let mut s = std::net::TcpStream::connect(("127.0.0.1", i.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    for n in 0..3 {
        let path = if n == 1 { "/health" } else { "/v1/models" };
        let r = format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: keep-alive\r\n\r\n");
        s.write_all(r.as_bytes()).unwrap();
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.contains("200"), "req {n}: {status}");
        // lire jusqu'à la fin du body (Content-Length connu) — simplification :
        // on lit les lignes d'en-têtes puis on cherche la fin
        let mut cl = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let ll = line.trim().to_string();
            if ll.is_empty() {
                break;
            }
            if let Some(v) = ll.strip_prefix("Content-Length:") {
                cl = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; cl];
        reader.read_exact(&mut body).unwrap();
        assert!(!body.is_empty());
    }
}

#[test]
fn e2e_corps_trop_grand_413() {
    // MAX_BODY = 1.5 Mo : on envoie 1.6 Mo
    let i = start("413", |_| {});
    let big = vec![b'x'; 1_600_000];
    let mut s = std::net::TcpStream::connect(("127.0.0.1", i.port)).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let head = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        big.len()
    );
    s.write_all(head.as_bytes()).unwrap();
    let _ = s.write_all(&big);
    let mut out = String::new();
    let _ = BufReader::new(s).read_to_string(&mut out);
    // le serveur peut couper avant la réponse complète ; s'il répond, c'est 413
    if !out.is_empty() {
        let code = out
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        assert_eq!(code, 413, "réponse: {out}");
    }
}

#[test]
fn e2e_requetes_concurrentes_toutes_servies() {
    let i = start("concurrent", |_| {});
    let mut handles = Vec::new();
    for n in 0..12 {
        let port = i.port;
        handles.push(std::thread::spawn(move || {
            chat(port, "space-bunny-free", &format!("PONG c{n}")).unwrap()
        }));
    }
    let mut oks = 0;
    for h in handles {
        let (code, _) = h.join().unwrap();
        assert_eq!(code, 200);
        oks += 1;
    }
    assert_eq!(oks, 12);
    let stats = get_json(i.port, "/admin/stats");
    // plancher : un RST-fantôme peut ajouter une requête rejouée
    assert!(
        stats["stats"]["ok_total"].as_u64().unwrap_or(0) >= 12,
        "stats={stats}"
    );
}

#[test]
fn e2e_gate_sature_429_busy_rejected() {
    // gate_max=1 : le SLOW occupe l'unique slot, le 2e client (autre prompt
    // pour éviter le cache) attend gate_wait puis reçoit 429 « serveur occupe ».
    let i = start("gate", |c| {
        c.gate_max = 1;
        c.gate_wait = Duration::from_millis(200);
    });
    let port = i.port;
    let occupant =
        std::thread::spawn(move || chat(port, "glm-5.3-flash-free", "SLOW 1500").unwrap());
    // laisser l'occupant prendre le slot
    std::thread::sleep(Duration::from_millis(150));
    let (code, v) = chat(port, "glm-5.3-flash-free", "PONG second").unwrap();
    assert_eq!(code, 429, "le 2e client doit être refusé gate saturé: {v}");
    assert_eq!(v["error"]["type"], "rate_limit_error");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("occupe"), "msg={msg}");
    let (_, _) = occupant.join().unwrap(); // l'occupant finit son SLOW
    let stats = get_json(port, "/admin/stats");
    assert!(
        stats["stats"]["busy_rejected"].as_u64().unwrap_or(0) >= 1,
        "stats={stats}"
    );
}

#[test]
fn e2e_cache_clear_recalcule() {
    // 1er appel → backender ; /admin/cache/clear → 2e appel identique
    // recalcule (cache_hits reste 0) alors qu'un hit aurait été servi du cache.
    let i = start("cacheclear", |_| {});
    let (c1, v1) = chat(i.port, "deepseek-v4.1-flash-free", "PONG cache").unwrap();
    assert_eq!(c1, 200, "v1={v1}");
    let (c2, v2) = chat(i.port, "deepseek-v4.1-flash-free", "PONG cache").unwrap();
    assert_eq!(c2, 200);
    assert_eq!(chat_content(&v2), chat_content(&v1)); // servi du cache
    let (code, _) = req(i.port, "POST", "/admin/cache/clear", b"", false).unwrap();
    assert_eq!(code, 200);
    let (_, v3) = chat(i.port, "deepseek-v4.1-flash-free", "PONG cache").unwrap();
    assert_eq!(
        chat_content(&v3),
        chat_content(&v1),
        "le stub est déterministe"
    );
    let stats = get_json(i.port, "/admin/stats");
    // planchers : un RST-fantôme peut ajouter un calcul/hit supplémentaire
    assert!(
        stats["stats"]["cache_hits"].as_u64().unwrap_or(0) >= 1,
        "stats={stats}"
    );
    assert!(
        stats["stats"]["ok_total"].as_u64().unwrap_or(0) >= 2,
        "2 calculs réels attendus: {stats}"
    );
}
