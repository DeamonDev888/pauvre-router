# Pauvre-Router 🗑️🚀

<p align="center">
  <img src="assets/banner.jpg" alt="Pauvre-Router 🗑️🚀" width="100%" />
</p>

<p align="center">
  <a href="#-key-features"><img src="https://img.shields.io/badge/Rust-std--only-f97316?style=for-the-badge&logo=rust&logoColor=white" alt="Rust std-only" /></a>
  <a href="#-key-features"><img src="https://img.shields.io/badge/Size-~650_KB-3b82f6?style=for-the-badge&logo=cargo&logoColor=white" alt="650KB binary" /></a>
  <a href="#-free-frontier-models--comparison-matrix"><img src="https://img.shields.io/badge/API_Budget-0_€_(100%25_Free)-10b981?style=for-the-badge&logo=free&logoColor=white" alt="100% Free" /></a>
  <a href="#-http-endpoints"><img src="https://img.shields.io/badge/Endpoints-Standard-8b5cf6?style=for-the-badge&logo=http&logoColor=white" alt="Standard Endpoints" /></a>
  <a href="#-free-frontier-models--comparison-matrix"><img src="https://img.shields.io/badge/Models-4_Frontier-ec4899?style=for-the-badge" alt="4 Frontier Models" /></a>
  <a href="#-license"><img src="https://img.shields.io/badge/License-MIT-green?style=for-the-badge" alt="MIT" /></a>
</p>

> **The local proxy that routes to OpenCode Zen's free frontier models** — for everyone who refuses to pay subscriptions, has zero API budget, and wants to run dozens of agents.

📖 *Documentation en français : [README.md](README.md)*

---

## 💡 Why this project?

Since **October 14, 2026**, OpenCode Zen requires the official OpenCode client's cryptographic fingerprint (specific TLS handshake + wire verification, anti-MITM / anti-replay).
Calling the API directly with plain `curl` or a standard client gets you blocked.

**Pauvre-Router** solves this elegantly:
1. It receives your requests on its standard `POST /v1/chat/completions` interface.
2. It delegates execution under the hood to the official `opencode run` binary, which passes the fingerprint check 100%.
3. It returns standard `chat.completions` flows, with caching, rate limiting and redundancy handled for you.

---

## 🏛️ Architecture & Request Flow

<p align="center">
  <img src="assets/architecture.svg" alt="Architecture & Request Flow" width="100%" />
</p>

---

## ✨ Key Features

<p align="center">
  <img src="assets/features-grid.svg" alt="Key Features" width="100%" />
</p>

- **Standard** — `POST /v1/chat/completions`, `GET /v1/models`: plug in any client (Hermes, scripts, curl, Continue.dev, VSCode…)
- **🧠 Smart Robin** — the `frontier-robin-free` pseudo-model is no longer a blind round-robin: it scores every frontier model by **EWMA latency** (failures and slow responses heavily penalized), sends traffic to the best, and keeps probing the others (1-in-5 exploration) so a recovered model is back in rotation within 5 requests. **Adaptive per-model timeouts** (1.5× EWMA) trigger fallbacks early on fast models and give slow ones real headroom.
- **⭐ GLM 5.3 Flash free** at the top of the cascade (frontier quality, live-tested)
- **2 routing modes** (hot switch via `/admin/mode`):
  - `priority` (default): **exhausts the best model** before falling over
  - `roundrobin`: spreads load evenly on every request
- **🔑 Always-on API key rotation** — the pool round-robins by default; a quota-blocked key gets a persistent 30 min cooldown, the next one takes over, **anonymous fallback** when no key is usable (never mixing keyed/anonymous, or Zen answers `Invalid credential`)
- **Exhaustion detection & quarantine** — classification (quota / rate-limit / invalid model / timeout) + **exponential-backoff cooldowns** (2 min → 1 h)
- **Solid anti-spam** — per-IP token bucket (burst 5, 20 req/min), 3 concurrent opencode spawns max, 1.5 MB body cap
- **Dedup cache** — identical prompts served from cache in 0 ms (TTL 10 min, LRU 128, in-house sha256)
- **Live admin** — `/admin/stats`, `/admin/mode`, `/admin/keys`, `/admin/update`, `/admin/reset`
- **Self-update awareness** — checks GitHub Releases at boot and hourly; `/health` and `/admin/stats` expose `update.available` + download URL as soon as a newer release exists
- **Auto persistence** — health + stats (60 s) and key cooldowns saved to disk
- **Rust std-only** — single ~650 KB binary, zero heavy dependencies (no tokio/reqwest)

---

## ⚡ Free Frontier Models & Comparison Matrix

<p align="center">
  <img src="assets/models-matrix.svg" alt="Frontier Models Matrix" width="100%" />
</p>

| Model | Latency | Specialty | Status |
|---|---|---|---|
| 🆕 `beam-free` | **5.3 s** ⚡ | **1M context**, 131K output, reasoning + tool calling — fastest of the four | 🟢 Free (limited time) |
| `space-bunny-free` | **5.7 s** ⚡ | The sprinter — ultra-fast for short questions and chat | 🟢 Free on Zen |
| `deepseek-v4.1-flash-free` | **9.0 s** | Versatile DeepSeek V4.1 — analysis & synthesis | 🟢 Free on Zen |
| ⭐ `glm-5.3-flash-free` | 6.4 s | **Cutting-edge frontier** — top quality for code and reasoning | 🟢 Free on Zen |
| 🔄 `frontier-robin-free` | *Dynamic* | **Master pseudo-model** — smart load balancing across the 4 frontiers | 🟣 Smart Load-Balancer |

> **Cascade order in priority mode:**  
> `beam-free` ➔ `glm-5.3-flash-free` ➔ `space-bunny-free` ➔ `deepseek-v4.1-flash-free`.  
> An explicitly requested model always goes first; `frontier-robin-free` routes across all four with smart scoring.

---

## 🔑 Key Rotation & Quota Resilience

<p align="center">
  <img src="assets/key-rotation.svg" alt="Key Rotation Cycle" width="100%" />
</p>

1. **Active pool** — your API keys round-robin to maximize throughput without burning out a single key.
2. **429 detection** — the moment a key returns an exhausted quota, it goes into immediate 30 min quarantine (persisted in `~/.pauvre-router-keys.json`).
3. **Instant failover** — the next key takes over with no client-visible error.
4. **Anonymous fallback** — when every key in the pool is quarantined, the router switches to anonymous Zen mode without mixing states.
5. **Progressive backoff** — repeated errors (rate-limit or timeout) escalate cooldowns 2 min ➔ 5 min ➔ 15 min ➔ 1 h.

---

## 🔌 HTTP Endpoints

| Endpoint | Method | Description |
|---|---|---|
| `/health` | `GET` | Liveness, compiled version, in-flight requests, update availability |
| `/v1/models` | `GET` | Available models (`frontier-robin-free` + the 4 frontiers) |
| `/v1/chat/completions` | `POST` | Standard chat (automatic cascade/mode) |
| `/admin/stats` | `GET` | Usage metrics, model health (EWMA latencies), anti-spam stats, active mode, update |
| `/admin/mode` | `GET` / `POST` | Inspect and hot-switch the routing mode (`priority` / `roundrobin`) |
| `/admin/keys` | `GET` | Key pool metadata (keys masked for safety) |
| `/admin/keys/add` | `POST` | Add an API key to the pool: `{"key": "sk-..."}` |
| `/admin/keys/remove` | `POST` | Remove an API key: `{"key": "sk-..."}` |
| `/admin/keys/reanimate`| `POST` | Reanimate a key or clear all cooldowns: `{}` or `{"key": "sk-..."}` |
| `/admin/update` | `GET` | Last known update check (current / latest / URL) |
| `/admin/update/check` | `POST` | Force an update check against GitHub Releases |
| `/admin/reset` | `POST` | Fully purge the dedup cache and model cooldowns |

---

## 🚀 Install & Run

Full multi-platform guide (Windows / Linux / macOS, auto-start service, Hermes plugin): **[SKILL.md](SKILL.md)**.  
Roadmap and upcoming work: **[TODO.md](TODO.md)**.

```bash
# Build the optimized binary (~650 KB)
cargo build --release

# Run
./target/release/pauvre-router
```

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `PAUVRE_OPENCODE` | Auto-detected path (Windows) | Absolute path to the `opencode` CLI (*required on Linux/macOS*) |
| `PAUVRE_STATE` | `~/.pauvre-router-state.json` | Persistence file for stats and model health |
| `PAUVRE_KEYS` | `~/.pauvre-router-keys.json` | Secure storage for the key pool and cooldowns |
| `PAUVRE_MODE` | `priority` | Default routing mode at boot (`priority` or `roundrobin`) |

---

## 🦹 Hermes Desktop Integration

Add this provider to your Hermes configuration (`config.yaml`):

```yaml
custom_providers:
- name: pauvre-router
  base_url: http://127.0.0.1:4445/v1
  api_key: pauvre
  api_mode: chat_completions
  models:
    frontier-robin-free: {}
    beam-free: {}
    glm-5.3-flash-free: {}
    space-bunny-free: {}
    deepseek-v4.1-flash-free: {}
```

> ⚠️ **Note:** the `models:` list in `config.yaml` is static — it's what feeds the Hermes model picker.

Result in the Hermes Desktop picker — the **PAUVRE-ROUTER** group with the 5 models, `space-bunny-free` selected (XHIGH context):

<p align="center">
  <img src="assets/hermes-picker.png" alt="pauvre-router models in the Hermes Desktop picker" width="85%" />
</p>

### 🖥️ Hermes Desktop Plugin

A real-time monitoring pane for Hermes (health, cooldowns, metrics, mode badge) ships with the project.  
See **[`SKILL.md`](SKILL.md) §3** for step-by-step setup.

---

## 📦 Releases

Windows, Linux and macOS binaries are built and packaged automatically by GitHub Actions on every tag:  
Head to the **[Releases](../../releases)** page for prebuilt binaries.

## 🧪 Tests

**84 tests** (59 unit + 6 backend stub + 19 E2E), all green in both `cargo test` and `cargo test --release`:

- **E2E** (`tests/e2e.rs`): real servers start on ephemeral ports (same workers as production) and actually speak TCP — an `opencode` stub (`src/bin/stub-opencode.rs`, driven by the `STUB_FAIL` env) replaces the real CLI. Coverage: fallback cascade, cooldowns, all-cooldown 429, token-bucket anti-spam, SSE streaming, cache, key rotation proven at spawn level, backend timeout, keep-alive, 413, state persistence across restart, 12 concurrent requests.

Full history: [CHANGELOG.md](CHANGELOG.md).

---

## 📜 License

Released under the **MIT** license — and yes, the name stays: *Pauvre-Router*. 🗑️🚀
