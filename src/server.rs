//! Router des Pauvres — cœur serveur.
//!
//! Boucle à workers FIXES : chaque worker prend une connexion de la file
//! d'attente et la sert JUSQU'À SA MORT (keep-alive, timeouts lecture/
//! écriture, deadline totale). Un client qui disparaît est récolté par
//! son worker — aucun thread par requête, aucun zombie possible.
//!
//! Tout ce qui varie entre la prod et les tests E2E est dans
//! [`ServerConfig`] : port, chemins d'état, exe du backend, timings.
//! Les tests démarrent de vrais serveurs sur des ports éphémères avec un
//! binaire stub — aucun réseau réel, aucune variable d'env de process.

use crate::backend;
use crate::cache::Cache;
use crate::classify::{self, ErrorKind};
use crate::guard::{Gate, RateLimiter};
use crate::http::{HttpConn, HttpError, Request, MAX_CONN_AGE};
use crate::keystore::{KeyChoice, KeyStore};
use crate::models::{
    cascade, pick, pick_robin_smart, pick_round_robin, Health, Registry, FREE_MODELS, ROBIN_MODEL,
};
use crate::prompt::{compose_prompt, extract_workdir, sanitize_model};
use crate::stats::{save_to, Stats};
use crate::update::Updater;
use crate::VERSION;

use std::collections::VecDeque;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_BODY: usize = 8_000_000; // 8 Mo — sessions Hermes lourdes (848 messages ≈ 1.2-1.5 Mo de JSON)
const KEY_BACKOFF: u64 = 1800; // cooldown clé après échec (30 min)
/// Workers fixes — LA réponse anti-zombie (aucun thread par connexion).
///
/// NB : un worker sert sa connexion JUSQU'À SA MORT (keep-alive : il
/// attend la requête suivante jusqu'à `http::MAX_IDLE`, et une requête
/// chat longue le retient jusqu'à 4 min). 8 workers suffisaient pour un
/// usage occasionnel, mais 8 connexions keep-alive INACTIVES (le cas
/// normal d'un SDK OpenAI qui garde son pool de sockets) =
/// 8 workers immobilisés = service MUET (plus un seul /health servi).
/// D'où 24 workers : la pression amont reste bornée par AILLEURS
/// (token-bucket par IP, budget anti-ban, gate backend à 3) — plus de
/// workers = plus de CONNEXIONS servies, jamais plus d'appels amont.
const N_WORKERS: usize = 24;
const QUEUE_CAP: usize = 64; // file de connexions en attente

/// Token d'administration par défaut (endpoint `/admin/*`). Surchargeable
/// par l'env `PAUVRE_ADMIN_TOKEN`. « pauvre » = la convention déjà
/// utilisée par Hermes (`api_key: pauvre`) et par la consigne injectée
/// au backend (src/prompt.rs).
pub const DEFAULT_ADMIN_TOKEN: &str = "pauvre";

/// Tout ce que le serveur lit hors du code. `ServerConfig::production()`
/// reproduit exactement le comportement historique du binaire ; les tests
/// E2E construisent la leur (port 0, tmpdir, stub, timings courts).
pub struct ServerConfig {
    pub port: u16,
    pub state_path: PathBuf,
    pub keys_path: PathBuf,
    /// Exe spawné par requête chat (prod: opencode, tests: stub).
    pub backend_exe: String,
    /// Env additionnelle posée sur chaque spawn backend (tests: pilote
    /// le stub ; prod: vide).
    pub backend_env: Vec<(String, String)>,
    /// Timeout d'un appel backend (par modèle).
    pub call_timeout: Duration,
    /// Anti-spam : capacité de rafale et régime permanent (jetons/min).
    pub rate_capacity: u32,
    pub rate_per_min: f64,
    /// Plafond de spawns backend simultanés et attente max d'un slot.
    pub gate_max: u32,
    pub gate_wait: Duration,
    /// Intervalle de sauvegarde périodique de l'état.
    pub save_interval: Duration,
    /// Mode de routage initial : "priority" | "roundrobin".
    pub mode: String,
    /// Anti-ban : budget d'appels backend/min (fenêtre glissante 60 s).
    pub antiban_budget: usize,
    /// Token d'administration (Bearer) exigé sur `/admin/*`. Vide = auth
    /// désactivée (déconseillé : les endpoints admin pilotent le pool de
    /// clés, l'anti-ban et le cache).
    pub admin_token: String,
}

impl ServerConfig {
    /// Config de production : identique au binaire historique.
    pub fn production() -> Self {
        let mode = std::env::var("PAUVRE_MODE").unwrap_or_else(|_| "priority".into());
        let keys_path = std::env::var("PAUVRE_KEYS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                    .join(".pauvre-router-keys.json")
            });
        Self {
            port: 4445,
            state_path: crate::stats::state_path(),
            keys_path,
            backend_exe: backend::opencode_path(),
            backend_env: Vec::new(),
            call_timeout: Duration::from_secs(80),
            rate_capacity: 5,
            rate_per_min: 20.0,
            gate_max: 3,
            gate_wait: Duration::from_secs(30),
            save_interval: Duration::from_secs(60),
            mode,
            antiban_budget: std::env::var("PAUVRE_BUDGET")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(crate::antiban::DEFAULT_BUDGET),
            admin_token: std::env::var("PAUVRE_ADMIN_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ADMIN_TOKEN.to_string()),
        }
    }
}

pub struct Shared {
    pub limiter: RateLimiter,
    pub gate: Gate,
    /// Attente max d'un slot backend (config).
    pub gate_wait: Duration,
    pub cache: Mutex<Cache>,
    pub registry: Mutex<Registry>,
    pub stats: Mutex<Stats>,
    pub in_flight: AtomicU64,
    /// Panics isolés dans la boucle des workers (observabilité : un
    /// compteur qui monte = autant de requêtes qui ont fait tomber le
    /// worker, sans tuer le service).
    pub panics: AtomicU64,
    pub spam_log: Mutex<Vec<(String, u64)>>,
    pub keystore: Mutex<KeyStore>,
    /// Mode de routage : "priority" | "roundrobin".
    pub mode: Mutex<String>,
    pub rr_cursor: Mutex<usize>,
    /// Chemin d'état (stats + santé) — périodiquement sauvegardé.
    pub state_path: PathBuf,
    /// Timeout d'un appel backend (par modèle).
    pub call_timeout: Duration,
    /// Env additionnelle des spawns backend (figée au boot).
    pub backend_env: Vec<(String, String)>,
    /// Détection de mise à jour (check au boot puis périodique).
    pub updater: Arc<Updater>,
    /// Exe backend, figé au boot (one-shot : Some au boot, jamais muté).
    pub backend_exe_cell: Mutex<Option<String>>,
    /// Anti-ban : budget global + circuit breaker flag IP.
    pub antiban: crate::antiban::AntiBan,
    /// Token d'administration (Bearer) exigé sur `/admin/*`.
    pub admin_token: String,
    /// Repo du service lui-même (découvert depuis current_exe au boot) —
    /// pour router les demandes auto-référentielles (« audit du pauvre
    /// router ») vers le VRAI repo au lieu du cwd du client.
    pub self_repo: Option<String>,
    /// Buffer mémoire des lignes de log (exposé via GET /admin/logs).
    pub log_buf: Mutex<crate::logbuf::LogBuffer>,
}

impl Shared {
    pub fn new(cfg: &ServerConfig) -> Self {
        Self {
            limiter: RateLimiter::new(cfg.rate_capacity, cfg.rate_per_min),
            gate: Gate::new(cfg.gate_max),
            gate_wait: cfg.gate_wait,
            cache: Mutex::new(Cache::default()),
            registry: Mutex::new(Registry::new()),
            stats: Mutex::new(Stats::default()),
            in_flight: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            spam_log: Mutex::new(Vec::new()),
            keystore: Mutex::new(KeyStore::open(cfg.keys_path.clone())),
            mode: Mutex::new(cfg.mode.clone()),
            rr_cursor: Mutex::new(0),
            state_path: cfg.state_path.clone(),
            call_timeout: cfg.call_timeout,
            backend_env: cfg.backend_env.clone(),
            backend_exe_cell: Mutex::new(Some(cfg.backend_exe.clone())),
            updater: Arc::new(Updater::new(VERSION.to_string())),
            antiban: crate::antiban::AntiBan::new(cfg.antiban_budget),
            admin_token: cfg.admin_token.clone(),
            self_repo: discover_self_repo(),
            log_buf: Mutex::new(crate::logbuf::LogBuffer::default()),
        }
    }
}

/// Écrit une ligne dans stderr ET dans le buffer mémoire (/admin/logs).
macro_rules! slog {
    ($shared:expr, $($arg:tt)*) => {{
        let line = format!($($arg)*);
        eprintln!("{line}");
        if let Ok(mut b) = $shared.log_buf.lock() {
            b.push(&line);
        }
    }};
}

/// Remonte depuis current_exe (…/pauvre-router/target/release/…) vers un
/// Cargo.toml contenant « pauvre-router » = le repo du service lui-même.
/// None si le binaire tourne hors de son repo (installé ailleurs).
fn discover_self_repo() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        let cargo = dir.join("Cargo.toml");
        if let Ok(txt) = std::fs::read_to_string(&cargo) {
            if txt.contains("pauvre-router") {
                return Some(dir.to_string_lossy().to_string());
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
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
        // verrous poison-tolérants : un mutex empoisonné (panic survenu
        // ailleurs) ne doit pas condamner la file entière.
        let mut q = crate::lock(&self.inner);
        loop {
            if let Some(s) = q.pop_front() {
                return s;
            }
            q = self.cv.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn push(&self, s: TcpStream) -> bool {
        let mut q = crate::lock(&self.inner);
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

/// Compteur « appels en vol » : décrémenté à la DROP (RAII). Un retour
/// anticipé OU un panic ne peut donc plus laisser `in_flight` collé
/// (le compteur est affiché par `/health` et `/admin/stats` : un
/// compteur figé fait croire à une saturation imaginaire).
struct InFlight<'a>(&'a AtomicU64);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
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

/// Le chemin cible-t-il l'espace d'administration ?
/// (`/admin` et `/admin/...` — pas `/administrateur`).
pub fn is_admin_path(path: &str) -> bool {
    path == "/admin" || path.starts_with("/admin/")
}

/// Le token d'administration est-il valide pour cette requête ?
/// Schéma `Authorization: Bearer <token>` (RFC 7235, schéma insensible
/// à la casse). Token vide = auth désactivée explicitement.
/// Comparaison à temps constant : on ne révèle pas le préfixe correct
/// par un retour anticipé.
fn admin_authorized(req: &Request, token: &str) -> bool {
    if token.is_empty() {
        return true;
    }
    let header = req.header("authorization").unwrap_or("").trim();
    let mut parts = header.split_whitespace();
    let scheme = parts.next().unwrap_or("");
    let value = parts.next().unwrap_or("").trim();
    if !scheme.eq_ignore_ascii_case("bearer") {
        return false;
    }
    let (a, b) = (value.as_bytes(), token.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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

/// Corps SSE (chat.completion.chunk) : le format que le client OpenAI
/// attend en streaming. Sans ça, Hermes lit 0 chunk → EmptyStreamError
/// → fallback provider. Rôle → contenu → fin → [DONE].
/// Le texte est découpé en chunks ~240 chars (frontières lignes/space) :
/// le client rend progressivement au lieu d'attendre la réponse entière.
/// Chunk SSE data: standard (rôle initial, delta de contenu, stop).
fn sse_chunk(
    id: &str,
    created: u64,
    model: &str,
    delta: serde_json::Value,
    finish: Option<&str>,
) -> String {
    let c = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    });
    format!("data: {c}\n\n")
}

/// Événement SSE de « progression » : un delta de contenu VIDE porteur
/// de finish_reason:null — un VRAI event data: que les watchdogs
/// OpenAI SDK comptent (contrairement aux commentaires `: ping`).
fn sse_progress(id: &str, created: u64, model: &str) -> String {
    sse_chunk(id, created, model, serde_json::json!({}), None)
}

fn sse_body(text: &str, model: &str) -> String {
    let id = format!("chatcmpl-pauvre-{}-{}", std::process::id(), now_epoch());
    let created = now_epoch();
    sse_body_with(&id, created, model, text, true)
}

/// Erreur TERMINALE d'un flux SSE DÉJÀ ouvert : payload `data:` portant
/// l'objet erreur (forme OpenAI), puis `[DONE]`, puis clôture du chunked.
///
/// Pourquoi : une fois le flux entamé (status line + préambule rôle
/// envoyés), la réponse HTTP est déjà partie — écrire une 2e réponse
/// HTTP brute (le chemin non-stream) CORROMPT le chunked en cours : le
/// client OpenAI voit une seconde status line et casse en « Connection
/// error » au lieu de lire une erreur exploitable. La forme
/// `data: {"error":{...}}` est celle que les SDK OpenAI savent lire
/// (événement d'erreur du flux).
fn sse_error_close(conn: &HttpConn, body: &serde_json::Value) {
    let payload = format!("data: {body}\n\ndata: [DONE]\n\n");
    let _ = conn.stream_append(|s| s.write_all(payload.as_bytes()));
    let _ = conn.stream_close();
}

/// Variante avec id/created imposés et rôle optionnel — la livraison sur
/// un flux déjà ouvert (keepalives) réutilise l'id du préambule et NE
/// réémet PAS le rôle (déjà parti au tout début).
fn sse_body_with(id: &str, created: u64, model: &str, text: &str, with_role: bool) -> String {
    let chunk = |delta: serde_json::Value, finish: Option<&str>| -> String {
        sse_chunk(id, created, model, delta, finish)
    };
    let mut out = String::new();
    if with_role {
        out.push_str(&chunk(
            serde_json::json!({"role": "assistant", "content": ""}),
            None,
        ));
    }
    for piece in split_stream_pieces(text) {
        out.push_str(&chunk(serde_json::json!({"content": piece}), None));
    }
    out.push_str(&chunk(serde_json::json!({}), Some("stop")));
    out.push_str("data: [DONE]\n\n");
    out
}

/// Découpe en morceaux ≈240 chars, frontière = fin de ligne ou espace.
/// Texte vide → aucun morceau (le rôle + stop suffisent).
fn split_stream_pieces(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    while start < text.len() {
        let end = (start + 240).min(text.len());
        // chercher une frontière propre dans la fenêtre [end-120, end)
        let cut = bytes[start..end]
            .iter()
            .rposition(|&b| b == b'\n' || b == b' ')
            .filter(|&p| p > 120) // pas de morceau ridiculement court
            .map(|p| start + p + 1)
            .unwrap_or(end);
        // ne jamais couper au milieu d'un caractère UTF-8
        let mut cut = cut.min(text.len());
        while cut > start && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        if cut == start {
            break;
        }
        pieces.push(text[start..cut].to_string());
        start = cut;
    }
    pieces
}

/// Répond la complétion : SSE si le client a demandé stream:true
/// (Hermes le fait toujours), JSON sinon (curl, tests).
fn respond_chat(conn: &mut HttpConn, text: &str, model: &str, stream: bool, keep: bool) {
    if stream {
        let body = sse_body(text, model);
        let _ = conn.write_stream(200, "OK", "text/event-stream", move |sink| {
            sink.write_all(body.as_bytes())
        });
    } else {
        let b = completion_body(text, model);
        let _ = conn.write_response(
            200,
            "OK",
            "application/json",
            b.to_string().as_bytes(),
            &[],
            keep,
        );
    }
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

    // ---- garde admin : token Bearer obligatoire sur /admin/* ----
    // Sans ce garde, n'importe quel PROCESSUS local — et surtout
    // n'importe quel SITE WEB ouvert dans le navigateur (POST
    // cross-origin « simple », sans preflight) — peut piloter le
    // service : `POST /admin/antiban {"release":true}` désactive la
    // protection anti-ban (celle qui existe depuis l'incident de ban
    // IP de 24 h du 18 oct 2026), `/admin/keys/add` injecte une clé,
    // `/admin/reset` vide cache + cooldowns. L'en-tête Authorization
    // est de plus refusé par le navigateur en cross-origin (preflight
    // → bloqué) : le CSRF « drive-by » est fermé.
    if path.starts_with("/admin") && !admin_authorized(&req, &shared.admin_token) {
        let b = err_body(
            "token admin invalide ou absent (Authorization: Bearer <token>)",
            "permission_error",
            None,
        );
        let _ = conn.write_response(
            401,
            "Unauthorized",
            "application/json",
            b.to_string().as_bytes(),
            &[("WWW-Authenticate", "Bearer")],
            keep,
        );
        return;
    }

    // ---- endpoints lecture ----
    if req.method == "GET" && (path == "/health" || path == "/v1/health") {
        let upd = shared.updater.status();
        let body = serde_json::json!({
            "status": "ok",
            "router": "pauvre-router",
            "version": VERSION,
            "mode": crate::lock(&shared.mode).clone(),
            "in_flight": shared.in_flight.load(Ordering::Relaxed),
            "panics": shared.panics.load(Ordering::Relaxed),
            "update": {
                "available": upd.update_available,
                "latest": upd.latest,
                "url": upd.url,
            },
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
    // ---- logs du service (queue du buffer mémoire) ----
    if req.method == "GET" && path == "/admin/logs" {
        let tail: usize = req
            .header("x-tail")
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let lines = shared.log_buf.lock().unwrap().tail(tail);
        let body = serde_json::json!({ "lines": lines, "tail": tail });
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
    // ---- mise à jour ----
    if req.method == "GET" && path == "/admin/update" {
        let upd = shared.updater.status();
        let body = serde_json::json!({
            "current": upd.current,
            "latest": upd.latest,
            "update_available": upd.update_available,
            "url": upd.url,
            "checked_at_epoch": upd.checked_at_epoch,
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
    if req.method == "POST" && path == "/admin/update/check" {
        // check forcé (6 s max côté GitHub) — la réponse attend
        let upd = shared.updater.check();
        let body = serde_json::json!({
            "current": upd.current,
            "latest": upd.latest,
            "update_available": upd.update_available,
            "url": upd.url,
            "checked_at_epoch": upd.checked_at_epoch,
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

    // ---- anti-ban ----
    if req.method == "GET" && path == "/admin/antiban" {
        let (used, flagged, blocked_secs) = shared.antiban.status();
        let body = serde_json::json!({
            "budget_per_min": shared.antiban.budget(),
            "backend_calls_this_window": used,
            "flagged": flagged,
            "blocked_secs": blocked_secs,
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
    if req.method == "POST" && path == "/admin/antiban" {
        let body: serde_json::Value =
            serde_json::from_slice(&req.body).unwrap_or(serde_json::json!({}));
        // release explicite : l'opérateur a changé de réseau (VPN off, autre
        // IP) et veut sortir du circuit sans attendre la sonde des 10 min
        let released = body
            .get("release")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if released {
            shared.antiban.release();
        }
        match body.get("budget").and_then(|v| v.as_u64()) {
            Some(b) => {
                let applied = shared.antiban.set_budget(b as usize);
                let body = serde_json::json!({
                    "budget_per_min": applied,
                    "released": released,
                    "applied": true,
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
            None => {
                if released {
                    let body = serde_json::json!({"released": true});
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
                        "champ 'budget' (u64) ou 'release' (bool) requis",
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
            }
        }
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
    let upd = shared.updater.status();
    let (antiban_used, antiban_flagged, antiban_blocked) = shared.antiban.status();
    let body = serde_json::json!({
        "version": VERSION,
        "stats": stats,
        "health": health,
        "recent_spam": recent_spam,
        "keys": keys_meta,
        "mode": mode,
        "in_flight": shared.in_flight.load(Ordering::Relaxed),
        "panics": shared.panics.load(Ordering::Relaxed),
        "antiban": {
            "budget_per_min": shared.antiban.budget(),
            "backend_calls_this_window": antiban_used,
            "flagged": antiban_flagged,
            "blocked_secs": antiban_blocked,
        },
        "update": {
            "available": upd.update_available,
            "latest": upd.latest,
            "url": upd.url,
            "checked_at_epoch": upd.checked_at_epoch,
        },
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
                } else if crate::keystore::is_valid_key_format(&key) {
                    // format valide mais déjà présent → rejeu RST idempotent
                    (
                        200,
                        serde_json::json!({"added": false, "total": ks.count()}),
                    )
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
                // idempotent : un rejeu RST peut retomber sur une clé déjà
                // supprimée — ce n'est pas une erreur, on répond 200 removed:false
                let removed = ks.remove(&key);
                if removed {
                    let _ = ks.save();
                }
                (
                    200,
                    serde_json::json!({"removed": removed, "total": ks.count()}),
                )
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
    let wants_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_tools = body
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let workdir = extract_workdir(&messages);
    // Auto-référence : « audit du pauvre router » avec le cwd du CLIENT
    // pointait l'agent sur le repo du client (vu en prod : il auditait
    // mcp-command-center). Si la question désigne CE service et que le
    // repo du service est connu, on IMPOSE son répertoire.
    let workdir = {
        let mentions_self = crate::prompt::last_user_text(&messages)
            .map(|q| {
                let l = q.to_lowercase();
                l.contains("pauvre")
                    || l.contains("pauvre-router")
                    || l.contains("router des pauvres")
            })
            .unwrap_or(false);
        match (&shared.self_repo, mentions_self) {
            (Some(repo), true) => Some(repo.clone()),
            _ => workdir,
        }
    };
    let prompt = match compose_prompt(&messages, has_tools, workdir.as_deref()) {
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
    // AVEC tools (agent build) : la tâche réelle (audit, corrections,
    // multi-fichiers) prend 2-4 min — timeout dédié, PAS le 80 s texte.
    let call_timeout = if has_tools {
        Duration::from_secs(240)
    } else {
        shared.call_timeout
    };

    // 3) cache
    if let Some(text) = shared.cache.lock().unwrap().get(&model_req, &prompt) {
        shared.stats.lock().unwrap().cache_hits += 1;
        respond_chat(conn, &text, &model_req, wants_stream, keep);
        return;
    }

    // 3.5) anti-ban : budget global d'appels backend + circuit flag IP.
    // L'incident du 18 oct 2026 (IP flaggée « abusive traffic » ~24 h) ne
    // doit plus pouvoir se reproduire : au-delà du budget, 429 immédiat
    // sans spawn ; circuit ouvert = refus sec aussi.
    if let Err(wait) = shared.antiban.acquire() {
        let kind = if shared.antiban.is_flagged() {
            "ip_flagged_error"
        } else {
            "rate_limit_error"
        };
        let msg = if shared.antiban.is_flagged() {
            "ip flaggee par l'amont (abusive traffic) : backend en pause, reessaie plus tard"
        } else {
            "budget backend atteint (protection anti-ban), reessaie plus tard"
        };
        let b = err_body(msg, kind, Some(wait.max(1)));
        let _ = conn.write_response(
            429,
            "Too Many Requests",
            "application/json",
            b.to_string().as_bytes(),
            &[("Retry-After", &wait.max(1).to_string())],
            keep,
        );
        return;
    }

    // 4) slot backend
    let _guard = match shared.gate.acquire(shared.gate_wait) {
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
    // garde RAII : le compteur redescend à la sortie de la fonction,
    // quel que soit le chemin (succès, erreur, retour anticipé, panic)
    let _inflight = InFlight(&shared.in_flight);

    // 5) cascade (robin = toute la chaîne frontière)
    let chain = cascade(&model_req);
    let mut last_err: Option<(serde_json::Value, u16)> = None;
    // SSE : préambule ouvert IMMÉDIATEMENT avec le RÔLE (data: rôle) —
    // c'est un VRAI chunk que les watchdogs OpenAI SDK comptent (les
    // commentaires `: ping` ne les apaisent pas). Le stream_id/created
    // sont figés ici pour que keepalives et réponse partagent le même id.
    let mut sse = wants_stream;
    // Le flux SSE est-il DÉJÀ ouvert (status line + rôle envoyés) ? Tant
    // qu'il l'est, TOUTE réponse doit être encodée en SSE (data:) — une
    // réponse HTTP brute écrite dans un chunked ouvert CORROMPT le flux
    // (2e status line → « Connection error » côté client OpenAI).
    let mut sse_open = false;
    let stream_id = format!("chatcmpl-pauvre-{}-{}", std::process::id(), now_epoch());
    let stream_created = now_epoch();
    if sse {
        let role = sse_chunk(
            &stream_id,
            stream_created,
            &model_req,
            serde_json::json!({"role": "assistant", "content": ""}),
            None,
        );
        let _ = conn.stream_open(200, "OK", "text/event-stream", |s| {
            s.write_all(role.as_bytes())
        });
        // le flux est entamé : dès lors, plus JAMAIS de HTTP brut
        sse_open = true;
    }
    for _attempt in 0..chain.len() {
        let now = Instant::now();
        // (modèle, timeout effectif) : le robin intelligent renvoie un
        // timeout adaptatif par modèle ; les autres chemins gardent la base.
        // AVEC tools (agent build) : timeout adaptatif désactivé — l'agent
        // travaille 30-60 s+ sur une vraie tâche, un timeout court couperait
        // un travail en cours ; on garde la base pleine.
        let (model, effective_timeout) = {
            let reg = shared.registry.lock().unwrap();
            let mode = shared.mode.lock().unwrap().clone();
            let picked = if model_req == ROBIN_MODEL && !has_tools {
                pick_robin_smart(
                    &chain,
                    &reg,
                    &mut shared.rr_cursor.lock().unwrap(),
                    now,
                    call_timeout,
                )
            } else if mode == "roundrobin" && !has_tools {
                pick_round_robin(&chain, &reg, &mut shared.rr_cursor.lock().unwrap(), now)
                    .map(|m| (m, call_timeout))
            } else if model_req == ROBIN_MODEL {
                // robin + tools : exploitation pure (pas d'exploration en
                // pleine tâche lourde), timeout = base pleine
                let reg2 = &reg;
                chain
                    .iter()
                    .find(|m| reg2.get(*m).map(|h| h.available(now)).unwrap_or(true))
                    .map(|m| (m.clone(), call_timeout))
            } else {
                pick(&chain, &reg, now).map(|m| (m, call_timeout))
            };
            match picked {
                Some(p) => p,
                None => {
                    if sse_open {
                        // flux SSE DÉJÀ ouvert : l'erreur part en payload
                        // data: (jamais de HTTP brut dans un flux ouvert)
                        let e = serde_json::json!({"error": {
                            "message": "tous les modeles frontieres sont en cooldown, reessaie plus tard",
                            "type": "rate_limit_error",
                            "retry_after": 30,
                        }});
                        sse_error_close(conn, &e);
                    } else {
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
                    }
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

        // Appel backend sur un THREAD DÉDIÉ + keepalive SSE : pendant que
        // l'agent opencode travaille (30 s-15 min), un ping SSE part toutes
        // les 5 s — le watchdog « premier chunk » du client ne tire jamais,
        // et le socket reste chaud. En non-stream, pas de keepalive possible
        // (la réponse doit partir d'un bloc) : timeout plein.
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let exe = shared_backend_exe(shared);
            let model_t = model.clone();
            let prompt_t = prompt.clone();
            let timeout_t = effective_timeout;
            let key_t = api_key.map(|s| s.to_string());
            let env_t = shared.backend_env.clone();
            let agent_t = has_tools;
            let dir_t = workdir.clone();
            std::thread::spawn(move || {
                let r = backend::call_at(
                    &exe,
                    &model_t,
                    &prompt_t,
                    timeout_t,
                    key_t.as_deref(),
                    &env_t,
                    agent_t,
                    dir_t.as_deref(),
                );
                let _ = tx.send(r);
            });
        }
        // SSE : le flux est déjà ouvert (préambule avant la cascade) —
        // alimenté en keepalives pendant le travail backend ci-dessous.
        // keepalive : tant que le backend n'a pas répondu, un VRAI event
        // data: (delta vide, finish null) part toutes les 5 s — les
        // watchdogs OpenAI SDK comptent les data:, pas les commentaires.
        // Client parti (erreur d'écriture) : on arrête de pinger mais on
        // ATTEND la fin du travail — le résultat va au cache (requête
        // identique suivante servie instantanément).
        let backend_result = if wants_stream {
            loop {
                match rx.recv_timeout(Duration::from_secs(5)) {
                    Ok(r) => break Some(r),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if sse {
                            let p = sse_progress(&stream_id, stream_created, &model_req);
                            if conn.stream_append(|s| s.write_all(p.as_bytes())).is_err() {
                                sse = false; // client parti : silence, on finit le travail
                            }
                        }
                    }
                    Err(_) => break None,
                }
            }
        } else {
            rx.recv().ok()
        };
        let backend_result = backend_result.unwrap_or_else(|| {
            // secours (canal fermé / thread paniqué) : erreur classée
            Err(backend::BackendError::Failed {
                raw: "backend thread paniqué".into(),
                elapsed: Duration::ZERO,
            })
        });
        match backend_result {
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
                if wants_stream {
                    // la réponse SSE est DÉJÀ entamée (rôle + keepalives) :
                    // le corps part via stream_append (SANS réémettre le
                    // rôle), la clôture via stream_close
                    let body =
                        sse_body_with(&stream_id, stream_created, &model_req, &ok.text, false);
                    if conn.stream_append(|s| s.write_all(body.as_bytes())).is_ok() {
                        let _ = conn.stream_close();
                    }
                } else {
                    let b = completion_body(&ok.text, &model_req);
                    let _ = conn.write_response(
                        200,
                        "OK",
                        "application/json",
                        b.to_string().as_bytes(),
                        &[],
                        keep,
                    );
                }
                // in_flight : la garde RAII `_inflight` le décrémente à la
                // sortie de la fonction (succès, erreur ou panic) — NE PAS
                // le faire aussi ici : double décrément => compteur wrapping
                // en u64::MAX dans /health.
                return;
            }
            Err(be) => {
                let raw = match &be {
                    backend::BackendError::Failed { raw, .. } => raw.clone(),
                    backend::BackendError::Timeout { .. } => "timeout".to_string(),
                };
                let kind = {
                    // classifier SEULEMENT la tête de la sortie : les vraies
                    // erreurs amont (quota, flag IP) sortent en tête ; le
                    // reste est la transcription des outils de l'agent, qui
                    // peut CONTENIR les chaînes sensibles (audit de
                    // classify.rs → « abusive traffic » littéral →
                    // auto-empoisonnement du circuit)
                    let head: String = raw.chars().take(400).collect();
                    classify::classify(&head)
                };
                if kind == ErrorKind::IpFlagged {
                    // circuit ouvert + cooldown amont prolongé : la gateway
                    // a flaggé l'IP — marteler ne fait qu'aggraver le flag
                    shared.antiban.flag();
                }
                {
                    let mut reg = shared.registry.lock().unwrap();
                    let cd = reg
                        .entry(model.clone())
                        .or_default()
                        .note_failure(kind, now);
                    slog!(
                        shared,
                        "[pauvre-router] model={model} key={} kind={} cd={}s raw={}",
                        if api_key.is_some() { "pool" } else { "anon" },
                        kind.label(),
                        cd.as_secs(),
                        raw.chars().take(160).collect::<String>()
                    );
                }
                if kind == ErrorKind::IpFlagged {
                    if let Some(ra) = classify::cooldown_secs(&raw) {
                        shared.antiban.cooldown_changed(ra);
                    }
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
                        &format!("modele {model} indisponible ({})", kind.label()),
                        "upstream_error",
                        None,
                    ),
                    502,
                ));
            }
        }
    }
    // in_flight : décrémenté par la garde RAII `_inflight` (à la sortie de
    // la fonction) — pas de double décrément ici.
    let (b, code) = last_err.unwrap_or_else(|| {
        (
            err_body("aucun modele disponible", "upstream_error", None),
            502,
        )
    });
    if sse_open {
        // Le flux SSE est DÉJÀ ouvert (préambule rôle + keepalives) :
        // l'erreur part en payload data: puis [DONE] + clôture. Écrire
        // ici une réponse HTTP brute (le chemin non-stream) CORROMPrait
        // le chunked en cours — le client OpenAI aurait vu une 2e
        // status line et cassé en « Connection error ».
        sse_error_close(conn, &b);
    } else {
        let _ = conn.write_response(
            code,
            "Bad Gateway",
            "application/json",
            b.to_string().as_bytes(),
            &[],
            keep,
        );
    }
}
fn shared_backend_exe(shared: &Shared) -> String {
    shared
        .backend_exe_cell
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default()
}

fn load_state(shared: &Shared) {
    let p = &shared.state_path;
    if let Ok(raw) = std::fs::read_to_string(p) {
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

fn periodic_save(shared: Arc<Shared>, interval: Duration) {
    loop {
        std::thread::sleep(interval);
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
            if let Err(e) = save_to(
                &shared.state_path,
                &stats,
                &serde_json::Value::Object(health),
            ) {
                eprintln!("[pauvre-router] save KO: {e}");
            }
        }
        let ks = shared.keystore.lock().unwrap();
        if let Err(e) = ks.save() {
            eprintln!("[pauvre-router] keystore save KO: {e}");
        }
    }
}

/// Serveur lié à son port — [`bind_server`] le crée, [`serve`] le fait
/// tourner (bloquant). Séparés pour que les tests puissent lire le port
/// réel (bind sur 0 = port éphémère) avant de lancer la boucle d'accept.
pub struct BoundServer {
    listener: TcpListener,
    /// Port réellement lié (utile quand config.port == 0).
    pub port: u16,
    /// 'static via leak : le serveur vit pour toujours ; en test, le
    /// process de test se termine et l'OS récupère.
    queue: &'static Arc<Queue>,
    shared: &'static Arc<Shared>,
}

/// Prépare l'état partagé, les workers et le listener (sans accepter).
pub fn bind_server(cfg: &ServerConfig) -> std::io::Result<BoundServer> {
    let shared = Arc::new(Shared::new(cfg));
    load_state(&shared);

    // détection de mise à jour : check au boot puis toutes les heures
    shared.updater.spawn_periodic();

    // sauvegarde périodique (stats + santé + cooldowns de clés)
    {
        let s = shared.clone();
        let interval = cfg.save_interval;
        std::thread::spawn(move || periodic_save(s, interval));
    }

    let listener = TcpListener::bind(("127.0.0.1", cfg.port))?;
    let port = listener.local_addr()?.port();
    let n_keys = shared.keystore.lock().unwrap().count();
    eprintln!(
        "[pauvre-router] v{VERSION} sur http://127.0.0.1:{port} | workers fixes: {N_WORKERS}, file: {QUEUE_CAP} | keys pool: {n_keys} | cascade: {}",
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
                // ISOLATION DU PANIC : le pool est FIXE (8 threads) — un
                // panic dans le traitement d'une requête (entrée client
                // NON FIABLE : system prompt arbitraire, JSON malformé)
                // tuait le thread pour TOUJOURS. 8 panics = service
                // muet (file pleine, connexions refermées) alors que le
                // binaire tournait encore. Le bug d'offset UTF-8 de
                // `parse_workdir_line` (8 requêtes suffisaient) l'a
                // prouvé en prod. Ici le panic est isolé : le worker
                // survit, la connexion concernée est refermée, le
                // compteur `panics` monte (visible dans /health).
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_conn(stream, peer, &s);
                }));
                if res.is_err() {
                    s.panics.fetch_add(1, Ordering::Relaxed);
                    eprintln!(
                        "[pauvre-router] panic isolé dans le worker {id} — worker vivant, service intact"
                    );
                }
            })
            .expect("spawn worker");
    }

    Ok(BoundServer {
        listener,
        port,
        queue: Box::leak(Box::new(queue)),
        shared: Box::leak(Box::new(shared)),
    })
}

/// Boucle d'accept — tourne à jamais (bloquant). Les workers consomment
/// la file ; en cas de saturation, la connexion est refermée aussitôt
/// (backpressure, aucun thread créé).
pub fn serve(bound: BoundServer) {
    let BoundServer {
        listener,
        queue,
        shared,
        ..
    } = bound;
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let _ = queue.push(stream);
            }
            Err(e) => eprintln!("[pauvre-router] accept KO: {e}"),
        }
    }
    let _ = shared; // gardé vivant par les workers ; ici pour la signature
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_split_decoupe_et_recompose() {
        let text = "ligne une\nligne deux\nligne trois avec pas mal de mots pour depasser la taille fenetre et verifier la coupure sur espace au milieu sinon on tronque au caractere pres sans jamais casser l utf8";
        let pieces = split_stream_pieces(text);
        assert!(pieces.len() >= 2, "doit decouper: {pieces:?}");
        assert_eq!(pieces.join(""), text, "recomposition exacte");
        for p in &pieces {
            assert!(p.len() <= 240, "pas de morceau geant: {}", p.len());
        }
    }

    #[test]
    fn sse_split_utf8_et_vide() {
        assert!(split_stream_pieces("").is_empty());
        let e = "🦀".repeat(600);
        let pieces = split_stream_pieces(&e);
        assert_eq!(pieces.join(""), e, "jamais de coupure utf8");
        for p in &pieces {
            assert!(std::str::from_utf8(p.as_bytes()).is_ok());
        }
    }

    #[test]
    fn err_body_structure_et_retry_after() {
        let simple = err_body("boom", "upstream_error", None);
        assert_eq!(simple["error"]["message"], "boom");
        assert_eq!(simple["error"]["type"], "upstream_error");
        assert!(simple["error"].get("retry_after").is_none());

        let avec = err_body("trop vite", "rate_limit_error", Some(7));
        assert_eq!(avec["error"]["retry_after"], 7);
    }

    #[test]
    fn completion_body_conforme_openai() {
        let b = completion_body("salut", "glm-5.3-flash-free");
        assert_eq!(b["object"], "chat.completion");
        assert_eq!(b["model"], "glm-5.3-flash-free");
        assert_eq!(b["choices"][0]["finish_reason"], "stop");
        assert_eq!(b["choices"][0]["message"]["role"], "assistant");
        assert_eq!(b["choices"][0]["message"]["content"], "salut");
        assert!(b["created"].as_u64().unwrap_or(0) > 0);
        assert_eq!(b["usage"]["total_tokens"], 0);
    }

    /// Décode un corps SSE en liste de payloads (dernier = "[DONE]").
    fn sse_payloads(body: &str) -> Vec<String> {
        body.split("\n\n")
            .filter(|l| l.starts_with("data: "))
            .map(|l| l.trim_start_matches("data: ").to_string())
            .collect()
    }

    #[test]
    fn sse_body_sequence_complete_hermes() {
        let body = sse_body("PONG", "space-bunny-free");
        let p = sse_payloads(&body);
        // rôle → contenu → stop → [DONE]
        assert_eq!(p.len(), 4, "body={body}");
        assert_eq!(p.last().unwrap(), "[DONE]");
        let chunks: Vec<serde_json::Value> = p[..3]
            .iter()
            .map(|j| serde_json::from_str(j).expect("chunk json valide"))
            .collect();
        for c in &chunks {
            assert_eq!(c["object"], "chat.completion.chunk");
            assert_eq!(c["model"], "space-bunny-free");
        }
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "PONG");
        assert_eq!(chunks[2]["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn sse_body_texte_vide_reste_valide() {
        let body = sse_body("", "m");
        let p = sse_payloads(&body);
        // pas de chunk contenu, mais rôle + stop + [DONE]
        assert_eq!(p.len(), 3);
        assert_eq!(p.last().unwrap(), "[DONE]");
        assert!(!body.contains("\"content\":\"\"},")); // pas de chunk contenu vide
    }

    #[test]
    fn config_production_valeurs_histriques() {
        let cfg = ServerConfig::production();
        assert_eq!(cfg.port, 4445);
        assert_eq!(cfg.call_timeout, Duration::from_secs(80));
        assert_eq!(cfg.rate_capacity, 5);
        assert_eq!(cfg.gate_max, 3);
        assert!(cfg.backend_env.is_empty());
    }

    #[test]
    fn shared_new_expose_la_config() {
        let dir = std::env::temp_dir().join(format!("pauvre-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ServerConfig {
            port: 0,
            state_path: dir.join("s.json"),
            keys_path: dir.join("k.json"),
            backend_exe: "nope.exe".into(),
            backend_env: vec![("STUB_FAIL".into(), "glm-5.3-flash-free".into())],
            call_timeout: Duration::from_millis(10),
            rate_capacity: 1,
            rate_per_min: 1.0,
            gate_max: 2,
            gate_wait: Duration::from_millis(3),
            save_interval: Duration::from_secs(9),
            mode: "roundrobin".into(),
            antiban_budget: 10_000,
            admin_token: String::new(),
        };
        let shared = Shared::new(&cfg);
        assert_eq!(shared.call_timeout, Duration::from_millis(10));
        assert_eq!(shared.gate_wait, Duration::from_millis(3));
        assert_eq!(shared.backend_env.len(), 1);
        assert_eq!(shared.mode.lock().unwrap().as_str(), "roundrobin");
        assert_eq!(shared_backend_exe(&shared), "nope.exe", "exe figé au boot");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
