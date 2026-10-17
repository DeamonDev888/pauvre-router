//! Router des Pauvres — serveur.
//!
//! Boucle à workers FIXES : chaque worker prend une connexion de la file
//! d'attente et la sert JUSQU'À SA MORT (keep-alive, timeouts lecture/
//! écriture, deadline totale). Un client qui disparaît est récolté par
//! son worker — aucun thread par requête, aucun zombie possible.
//!
//! POST /v1/chat/completions → spawn de `opencode run` (le client
//! officiel passe la gate fingerprint Zen) avec cascade de fallback,
//! pool de clés API en rotation, anti-spam, cache et cooldowns.
use pauvre_router::backend;
use pauvre_router::cache::Cache;
use pauvre_router::classify::{self, ErrorKind};
use pauvre_router::guard::{Gate, RateLimiter};
use pauvre_router::http::{HttpConn, HttpError, Request, MAX_CONN_AGE};
use pauvre_router::keystore::{KeyChoice, KeyStore};
use pauvre_router::models::{
    cascade, pick, pick_round_robin, Health, Registry, FREE_MODELS, ROBIN_MODEL,
};
use pauvre_router::prompt::{compose_prompt, sanitize_model};
use pauvre_router::stats::{save, Stats};
use pauvre_router::VERSION;

use std::collections::VecDeque;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PORT: u16 = 4445;
const MAX_BODY: usize = 1_500_000; // 1.5 Mo
const RATE_CAPACITY: u32 = 5; // rafale
const RATE_PER_MIN: f64 = 20.0; // régime permanent
const GATE_MAX: u32 = 3; // spawns opencode simultanés max
const GATE_WAIT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(80); // par modèle — un client ne doit jamais attendre des plafonds
const KEY_BACKOFF: u64 = 1800; // cooldown clé après échec (30 min)
const N_WORKERS: usize = 8; // workers fixes — LA réponse anti-zombie
const QUEUE_CAP: usize = 64; // file de connexions en attente

struct Shared {
    limiter: RateLimiter,
    gate: Gate,
    cache: Mutex<Cache>,
    registry: Mutex<Registry>,
    stats: Mutex<Stats>,
    in_flight: AtomicU64,
    spam_log: Mutex<Vec<(String, u64)>>,
    keystore: Mutex<KeyStore>,
    /// Mode de routage : "priority" | "roundrobin".
    mode: Mutex<String>,
    rr_cursor: Mutex<usize>,
}

/// File de connexions bornée (producteur = accept, consommateurs = workers).
struct Queue {
    inner: Mutex<VecDeque<TcpStream>>,
    cv: Condvar,
}

impl Queue {
    fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(cap)),
            cv: Condvar::new(),
        }
    }

    /// Bloque jusqu'à une connexion disponible.
    fn pop(&self) -> TcpStream {
        let mut q = self.inner.lock().unwrap();
        loop {
            if let Some(s) = q.pop_front() {
                return s;
            }
            q = self.cv.wait(q).unwrap();
        }
    }

    fn push(&self, s: TcpStream) -> bool {
        let mut q = self.inner.lock().unwrap();
        if q.len() >= QUEUE_CAP {
            return false; // saturation : on referme (backpressure)
        }
        q.push_back(s);
        drop(q);
        self.cv.notify_one();
        true
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn err_body(msg: &str, kind: &str, retry_after: Option<u64>) -> serde_json::Value {
    let mut e = serde_json::json!({
        "error": {"message": msg, "type": kind}
    });
    if let Some(ra) = retry_after {
        e["error"]["retry_after"] = serde_json::json!(ra);
    }
    e
}

fn completion_body(text: &str, model: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("pauvre-{}-{}", std::process::id(), now_epoch()),
        "object": "chat.completion",
        "created": now_epoch(),
        "model": model,
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": text},
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })
}

/// Sert UNE connexion entière (1..n requêtes en keep-alive) sous deadline.
fn serve_conn(stream: TcpStream, peer: std::net::IpAddr, shared: &Arc<Shared>) {
    let peer_str = peer.to_string();
    let mut conn = match HttpConn::new(stream) {
        Ok(c) => c,
        Err(_) => return,
    };
    let deadline = Instant::now() + MAX_CONN_AGE;
    loop {
        let idle_left = deadline.saturating_duration_since(Instant::now());
        if idle_left.is_zero() {
            return; // connexion trop vieille : fermée proprement
        }
        let req = match conn.read_request(MAX_BODY, idle_left) {
            Ok(r) => r,
            Err(HttpError::Closed) | Err(HttpError::ConnExpired) => return,
            Err(HttpError::TooLarge) => {
                let b = err_body("body trop grand", "invalid_request_error", None);
                let _ = conn.write_response(
                    413,
                    "Payload Too Large",
                    "application/json",
                    b.to_string().as_bytes(),
                    &[],
                    false,
                );
                return;
            }
            Err(HttpError::BadRequest(m)) => {
                let b = err_body(&m, "invalid_request_error", None);
                let _ = conn.write_response(
                    400,
                    "Bad Request",
                    "application/json",
                    b.to_string().as_bytes(),
                    &[],
                    false,
                );
                return;
            }
            Err(HttpError::Io(_)) => return,
        };
        let keep = req.keep_alive && Instant::now() < deadline;
        route_request(&mut conn, req, peer, &peer_str, shared, keep);
        if !keep {
            return;
        }
    }
}

/// Route une requête unique. `keep` est répercuté sur la réponse.
fn route_request(
    conn: &mut HttpConn,
    req: Request,
    peer: std::net::IpAddr,
    peer_str: &str,
    shared: &Arc<Shared>,
    keep: bool,
) {
    let path = req.path.split('?').next().unwrap_or("").to_string();

    if req.method == "OPTIONS" {
        let _ = conn.write_response(204, "No Content", "text/plain", b"", &[], keep);
        return;
    }

    // ---- endpoints lecture ----
    if req.method == "GET" && (path == "/health" || path == "/v1/health") {
        let body = serde_json::json!({
            "status": "ok",
            "router": "pauvre-router",
            "version": VERSION,
            "mode": shared.mode.lock().unwrap().clone(),
            "in_flight": shared.in_flight.load(Ordering::Relaxed),
        });
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }
    if req.method == "GET" && (path == "/v1/models" || path == "/models") {
        let mut models = vec![serde_json::json!({
            "id": ROBIN_MODEL,
            "object": "model",
            "description": "round-robin permanent sur les modeles frontieres",
        })];
        models.extend(
            FREE_MODELS
                .iter()
                .map(|m| serde_json::json!({"id": m, "object": "model"})),
        );
        let body = serde_json::json!({"object": "list", "data": models});
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }
    if req.method == "GET" && (path == "/admin/stats" || path == "/admin/health") {
        admin_stats(conn, shared, keep);
        return;
    }
    if req.method == "POST" && (path == "/admin/cache/clear" || path == "/admin/reset") {
        if let Ok(mut c) = shared.cache.lock() {
            c.clear();
        }
        if let Ok(mut reg) = shared.registry.lock() {
            reg.clear();
        }
        let body = serde_json::json!({"cleared": true});
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }

    // ---- mode de routage ----
    if req.method == "GET" && path == "/admin/mode" {
        let m = shared.mode.lock().unwrap().clone();
        let body = serde_json::json!({"mode": m});
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }
    if req.method == "POST" && path == "/admin/mode" {
        let body: serde_json::Value =
            serde_json::from_slice(&req.body).unwrap_or(serde_json::json!({}));
        let wanted = body.get("mode").and_then(|v| v.as_str()).unwrap_or("");
        if wanted == "priority" || wanted == "roundrobin" {
            *shared.mode.lock().unwrap() = wanted.to_string();
            let body = serde_json::json!({"mode": wanted, "applied": true});
            let _ = conn.write_response(
                200,
                "OK",
                "application/json",
                body.to_string().as_bytes(),
                &[],
                keep,
            );
        } else {
            let b = err_body(
                "mode invalide (priority|roundrobin)",
                "invalid_request_error",
                None,
            );
            let _ = conn.write_response(
                400,
                "Bad Request",
                "application/json",
                b.to_string().as_bytes(),
                &[],
                keep,
            );
        }
        return;
    }

    // ---- pool de clés API ----
    if req.method == "GET" && path == "/admin/keys" {
        let body = shared.keystore.lock().unwrap().meta();
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            body.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }
    if req.method == "POST" && path.starts_with("/admin/keys/") {
        admin_keys(conn, req, &path, shared, keep);
        return;
    }

    // ---- chat ----
    if req.method == "POST" && (path == "/v1/chat/completions" || path == "/chat/completions") {
        handle_chat(req, peer, peer_str, shared, conn, keep);
        return;
    }

    let b = err_body("endpoint inconnu", "invalid_request_error", None);
    let _ = conn.write_response(
        404,
        "Not Found",
        "application/json",
        b.to_string().as_bytes(),
        &[],
        keep,
    );
}

fn admin_stats(conn: &mut HttpConn, shared: &Arc<Shared>, keep: bool) {
    let stats = shared.stats.lock().unwrap().clone();
    let reg = shared.registry.lock().unwrap();
    let mut health = serde_json::Map::new();
    let now = Instant::now();
    for m in FREE_MODELS {
        let h = reg.get(*m);
        let (cd, fails, oks, lat) = match h {
            Some(h) => (
                h.cooldown_until
                    .and_then(|t| t.checked_duration_since(now))
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                h.failures,
                h.ok_count,
                h.last_latency_ms,
            ),
            None => (0, 0, 0, 0),
        };
        health.insert(
            (*m).to_string(),
            serde_json::json!({"cooldown_secs": cd, "failures": fails, "ok": oks, "last_latency_ms": lat}),
        );
    }
    drop(reg);
    let spam = shared.spam_log.lock().unwrap();
    let recent_spam: Vec<_> = spam
        .iter()
        .rev()
        .take(20)
        .map(|(ip, ts)| serde_json::json!({"ip": ip, "at_epoch": ts}))
        .collect();
    drop(spam);
    let keys_meta = shared.keystore.lock().unwrap().meta();
    let mode = shared.mode.lock().unwrap().clone();
    let body = serde_json::json!({
        "version": VERSION,
        "stats": stats,
        "health": health,
        "recent_spam": recent_spam,
        "keys": keys_meta,
        "mode": mode,
        "in_flight": shared.in_flight.load(Ordering::Relaxed),
    });
    let _ = conn.write_response(
        200,
        "OK",
        "application/json",
        body.to_string().as_bytes(),
        &[],
        keep,
    );
}

fn admin_keys(conn: &mut HttpConn, req: Request, path: &str, shared: &Arc<Shared>, keep: bool) {
    let action = path.trim_start_matches("/admin/keys/");
    let body: serde_json::Value =
        serde_json::from_slice(&req.body).unwrap_or(serde_json::json!({}));
    let key = body
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (code, body) = {
        let mut ks = shared.keystore.lock().unwrap();
        match action {
            "add" => {
                if key.is_empty() {
                    (
                        400,
                        err_body("champ 'key' requis", "invalid_request_error", None),
                    )
                } else if ks.add(&key) {
                    let _ = ks.save();
                    (200, serde_json::json!({"added": true, "total": ks.count()}))
                } else {
                    (
                        400,
                        err_body(
                            "cle refusee (doublon ou format invalide: prefixe sk-/zen-, 20-200 chars)",
                            "invalid_request_error",
                            None,
                        ),
                    )
                }
            }
            "remove" => {
                if ks.remove(&key) {
                    let _ = ks.save();
                    (
                        200,
                        serde_json::json!({"removed": true, "total": ks.count()}),
                    )
                } else {
                    (404, err_body("cle inconnue", "invalid_request_error", None))
                }
            }
            "reanimate" => {
                if key.is_empty() {
                    let n = ks.count();
                    ks.reanimate_all();
                    let _ = ks.save();
                    (200, serde_json::json!({"reanimated": n}))
                } else {
                    ks.note_success(&key);
                    let _ = ks.save();
                    (200, serde_json::json!({"reanimated": 1}))
                }
            }
            _ => (
                404,
                err_body(
                    "action inconnue (add|remove|reanimate)",
                    "invalid_request_error",
                    None,
                ),
            ),
        }
    };
    let reason = if code == 200 { "OK" } else { "Bad Request" };
    let _ = conn.write_response(
        code,
        reason,
        "application/json",
        body.to_string().as_bytes(),
        &[],
        keep,
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_chat(
    req: Request,
    peer: std::net::IpAddr,
    peer_str: &str,
    shared: &Arc<Shared>,
    conn: &mut HttpConn,
    keep: bool,
) {
    // 1) anti-spam
    if let Some(w) = shared.limiter.check(peer).err() {
        shared.stats.lock().unwrap().spam_blocked += 1;
        shared
            .spam_log
            .lock()
            .unwrap()
            .push((peer_str.to_string(), now_epoch()));
        let body = err_body(
            &format!("anti-spam: trop de requetes, reessaie dans {w}s"),
            "rate_limit_error",
            Some(w),
        );
        let _ = conn.write_response(
            429,
            "Too Many Requests",
            "application/json",
            body.to_string().as_bytes(),
            &[("Retry-After", &w.to_string())],
            keep,
        );
        return;
    }

    // 2) parse
    let body: serde_json::Value = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(e) => {
            let b = err_body(
                &format!("json invalide: {e}"),
                "invalid_request_error",
                None,
            );
            let _ = conn.write_response(
                400,
                "Bad Request",
                "application/json",
                b.to_string().as_bytes(),
                &[],
                keep,
            );
            return;
        }
    };
    let model_req = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("glm-5.3-flash-free")
        .to_string();
    if !sanitize_model(&model_req) {
        let b = err_body("nom de modele invalide", "invalid_request_error", None);
        let _ = conn.write_response(
            400,
            "Bad Request",
            "application/json",
            b.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }
    let messages = body
        .get("messages")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let prompt = match compose_prompt(&messages) {
        Ok(p) => p,
        Err(e) => {
            let b = err_body(&e, "invalid_request_error", None);
            let _ = conn.write_response(
                400,
                "Bad Request",
                "application/json",
                b.to_string().as_bytes(),
                &[],
                keep,
            );
            return;
        }
    };

    // 3) cache
    if let Some(text) = shared.cache.lock().unwrap().get(&model_req, &prompt) {
        shared.stats.lock().unwrap().cache_hits += 1;
        let b = completion_body(&text, &model_req);
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            b.to_string().as_bytes(),
            &[],
            keep,
        );
        return;
    }

    // 4) slot backend
    let _guard = match shared.gate.acquire(GATE_WAIT) {
        Some(g) => g,
        None => {
            shared.stats.lock().unwrap().busy_rejected += 1;
            let b = err_body("serveur occupe, reessaie", "rate_limit_error", Some(5));
            let _ = conn.write_response(
                429,
                "Too Many Requests",
                "application/json",
                b.to_string().as_bytes(),
                &[("Retry-After", "5")],
                keep,
            );
            return;
        }
    };
    shared.in_flight.fetch_add(1, Ordering::Relaxed);

    // 5) cascade (robin = toute la chaîne frontière)
    let chain = cascade(&model_req);
    let mut last_err: Option<(serde_json::Value, u16)> = None;
    for _attempt in 0..chain.len() {
        let now = Instant::now();
        let model = {
            let reg = shared.registry.lock().unwrap();
            let mode = shared.mode.lock().unwrap().clone();
            let picked = if model_req == ROBIN_MODEL || mode == "roundrobin" {
                let mut cur = shared.rr_cursor.lock().unwrap();
                pick_round_robin(&chain, &reg, &mut cur, now)
            } else {
                pick(&chain, &reg, now)
            };
            match picked {
                Some(m) => m,
                None => {
                    let b = err_body(
                        "tous les modeles frontieres sont en cooldown, reessaie plus tard",
                        "rate_limit_error",
                        Some(30),
                    );
                    let _ = conn.write_response(
                        429,
                        "Too Many Requests",
                        "application/json",
                        b.to_string().as_bytes(),
                        &[],
                        keep,
                    );
                    shared.in_flight.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            }
        };
        // rotation de clé : round-robin parmi les actives, anonyme sinon
        let key_choice = shared.keystore.lock().unwrap().select();
        let api_key: Option<&str> = match &key_choice {
            KeyChoice::Use(k) => Some(k.as_str()),
            KeyChoice::Anonymous => None,
        };
        let is_fallback = model != model_req && model_req != ROBIN_MODEL;

        match backend::call(&model, &prompt, CALL_TIMEOUT, api_key) {
            Ok(ok) => {
                let lat = ok.elapsed;
                {
                    let mut reg = shared.registry.lock().unwrap();
                    reg.entry(model.clone()).or_default().note_success(lat);
                }
                if let KeyChoice::Use(k) = &key_choice {
                    shared.keystore.lock().unwrap().note_success(k);
                }
                {
                    let mut s = shared.stats.lock().unwrap();
                    if is_fallback {
                        s.note_fallback(&model);
                    } else {
                        s.note_ok(&model);
                    }
                }
                shared
                    .cache
                    .lock()
                    .unwrap()
                    .put(&model_req, &prompt, &ok.text);
                let b = completion_body(&ok.text, &model_req);
                let _ = conn.write_response(
                    200,
                    "OK",
                    "application/json",
                    b.to_string().as_bytes(),
                    &[],
                    keep,
                );
                shared.in_flight.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            Err(be) => {
                let raw = match &be {
                    backend::BackendError::Failed { raw, .. } => raw.clone(),
                    backend::BackendError::Timeout { .. } => "timeout".to_string(),
                };
                let kind = classify::classify(&raw);
                {
                    let mut reg = shared.registry.lock().unwrap();
                    let cd = reg
                        .entry(model.clone())
                        .or_default()
                        .note_failure(kind, now);
                    eprintln!(
                        "[pauvre-router] model={model} key={} kind={kind:?} cd={}s raw={}",
                        if api_key.is_some() { "pool" } else { "anon" },
                        cd.as_secs(),
                        raw.chars().take(160).collect::<String>()
                    );
                }
                // clé invalidée seulement si le refus vient du compte (pas du modèle)
                if let KeyChoice::Use(k) = &key_choice {
                    if kind != ErrorKind::ModelInvalid {
                        let mut ks = shared.keystore.lock().unwrap();
                        ks.note_failure(k, kind, KEY_BACKOFF);
                        let _ = ks.save();
                    }
                }
                {
                    let mut s = shared.stats.lock().unwrap();
                    s.note_error(&model, kind == ErrorKind::Exhausted);
                }
                last_err = Some((
                    err_body(
                        &format!("modele {model} indisponible ({kind:?})"),
                        "upstream_error",
                        None,
                    ),
                    502,
                ));
            }
        }
    }
    shared.in_flight.fetch_sub(1, Ordering::Relaxed);
    let (b, code) = last_err.unwrap_or_else(|| {
        (
            err_body("aucun modele disponible", "upstream_error", None),
            502,
        )
    });
    let _ = conn.write_response(
        code,
        "Bad Gateway",
        "application/json",
        b.to_string().as_bytes(),
        &[],
        keep,
    );
}

fn load_state(shared: &Shared) {
    let p = pauvre_router::stats::state_path();
    if let Ok(raw) = std::fs::read_to_string(&p) {
        if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(s) = doc.get("stats") {
                if let Ok(parsed) = serde_json::from_value::<Stats>(s.clone()) {
                    *shared.stats.lock().unwrap() = parsed;
                }
            }
            if let Some(h) = doc.get("health").and_then(|v| v.as_object()) {
                let mut reg = shared.registry.lock().unwrap();
                for (k, v) in h {
                    let failures = v.get("failures").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    let cd_secs = v.get("cooldown_secs").and_then(|x| x.as_u64()).unwrap_or(0);
                    let cooldown_until =
                        (cd_secs > 0).then(|| Instant::now() + Duration::from_secs(cd_secs));
                    let hh = Health {
                        failures,
                        cooldown_until,
                        ..Default::default()
                    };
                    reg.insert(k.clone(), hh);
                }
            }
            eprintln!("[pauvre-router] etat restaure depuis {}", p.display());
        }
    }
}

fn periodic_save(shared: Arc<Shared>) {
    loop {
        std::thread::sleep(Duration::from_secs(60));
        let stats = shared.stats.lock().unwrap().clone();
        {
            let reg = shared.registry.lock().unwrap();
            let mut health = serde_json::Map::new();
            let now = Instant::now();
            for m in FREE_MODELS {
                let empty = Health::default();
                let h = reg.get(*m).unwrap_or(&empty);
                health.insert(
                    (*m).to_string(),
                    serde_json::json!({
                        "cooldown_secs": h.cooldown_until.and_then(|t| t.checked_duration_since(now)).map(|d| d.as_secs()).unwrap_or(0),
                        "failures": h.failures,
                    }),
                );
            }
            drop(reg);
            if let Err(e) = save(&stats, &serde_json::Value::Object(health)) {
                eprintln!("[pauvre-router] save KO: {e}");
            }
        }
        let ks = shared.keystore.lock().unwrap();
        if let Err(e) = ks.save() {
            eprintln!("[pauvre-router] keystore save KO: {e}");
        }
    }
}

fn keys_path() -> std::path::PathBuf {
    std::env::var("PAUVRE_KEYS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                .join(".pauvre-router-keys.json")
        })
}

fn main() {
    let routing_mode = std::env::var("PAUVRE_MODE").unwrap_or_else(|_| "priority".into());
    let keystore = KeyStore::open(keys_path());
    let n_keys = keystore.count();
    let shared = Arc::new(Shared {
        limiter: RateLimiter::new(RATE_CAPACITY, RATE_PER_MIN),
        gate: Gate::new(GATE_MAX),
        cache: Mutex::new(Cache::default()),
        registry: Mutex::new(Registry::new()),
        stats: Mutex::new(Stats::default()),
        in_flight: AtomicU64::new(0),
        spam_log: Mutex::new(Vec::new()),
        keystore: Mutex::new(keystore),
        mode: Mutex::new(routing_mode),
        rr_cursor: Mutex::new(0),
    });
    load_state(&shared);

    // sauvegarde périodique (stats + santé + cooldowns de clés)
    {
        let s = shared.clone();
        std::thread::spawn(move || periodic_save(s));
    }

    let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[pauvre-router] bind 127.0.0.1:{PORT} KO: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "[pauvre-router] v{VERSION} sur http://127.0.0.1:{PORT} | workers fixes: {N_WORKERS}, file: {QUEUE_CAP} | keys pool: {n_keys} | cascade: {}",
        FREE_MODELS.join(" -> ")
    );

    // worker pool fixe — la réponse aux zombies : N threads, pas un de plus
    let queue = Arc::new(Queue::new(QUEUE_CAP));
    for id in 0..N_WORKERS {
        let q = queue.clone();
        let s = shared.clone();
        std::thread::Builder::new()
            .name(format!("pauvre-worker-{id}"))
            .spawn(move || loop {
                let stream = q.pop();
                let peer = stream
                    .peer_addr()
                    .map(|a| a.ip())
                    .unwrap_or_else(|_| "127.0.0.1".parse().unwrap());
                serve_conn(stream, peer, &s);
            })
            .expect("spawn worker");
    }

    // accept boucle : push dans la file ; saturation => close immédiat
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if !queue.push(stream) {
                    // file pleine : répondre vite et fermer (pas de thread créé)
                }
            }
            Err(e) => eprintln!("[pauvre-router] accept KO: {e}"),
        }
    }
}

// (fin — les workers bouclent à jamais sur la file ; l'accept jamais)
