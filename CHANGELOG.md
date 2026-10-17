# Changelog

Toutes les évolutions notables de **Le Router des Pauvres**.
Format basé sur [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) — sémantique [SemVer](https://semver.org/).

## [0.5.1] — 2026-10-17

### Corrigé
- **Deadlock de pipe backend (critique, vu en prod)** : stdout/stderr de `opencode run` étaient lus *après* la fin du processus — une réponse longue remplissait le buffer du pipe, l'enfant bloquait en écriture, ne se terminait jamais, et la requête mourait au timeout (80 s). Symptômes observés : `in_flight` collé, latences 23 s+, erreurs en cascade sur toute la cascade. Le drain se fait maintenant **en threads pendant l'attente** — le deadlock est structurellement impossible. Prouvé en prod : 5 requêtes lourdes simultanées servies en 11-21 s, zéro échec, `in_flight` retombe à 0.

## [0.5.0] — 2026-10-17

### Ajouté
- **`beam-free` en tête de cascade** — Reflection Beam 1.0 (501B A23B) via OpenCode Zen : **1M de contexte**, 131K output, raisonnement + tool calling, et **le plus rapide des 4** (5,3 s au bench live). Gratuit pour une durée limitée.
- `frontier-robin-free` tourne sur les **4** frontières (was 3) : `beam` ➔ `glm` ➔ `space-bunny` ➔ `deepseek`.

### Modifié
- Provider Hermes renommé `pauvre-router` (api_key placeholder `pauvre`), configs overmind/kali alignées.
- README : titre « **Le** Router des Pauvres », intro élargie (« lancer des dizaines d'agents »), retrait du positionnement OpenAI-compat, suppression du graphique mermaid « Séquence d'une Requête », matrice des modèles re-benchée en live.

## [0.4.0] — 2026-10-16

### Ajouté
- **+14 tests (73 total)** : suite serveur (corps JSON/SSE conformes au client OpenAI d'Hermes), tests `call_at` contre le binaire stub réel (`tests/backend_stub.rs`), E2E gate saturé (`429 busy_rejected`) et `cache clear`.
- Suite E2E blindée : retry client anti-RST loopback Windows (×3, backoff) + assertions de compteurs en planchers (un RST peut rejouer un POST déjà servi — cache hit fantôme).

### Modifié
- Provider renommé `router-des-pauvres` (ex `zen-free`), placeholders `api_key: pauvre`.
- Plugin desktop aligné sur la version du crate.

## [0.3.3] — 2026-10-16

### Ajouté
- **Suite E2E complète (16 tests)** : de vrais serveurs TCP sur ports éphémères (mêmes 8 workers que la prod), stub `opencode` piloté par env `STUB_FAIL`. Couverture : cascade de fallback, cooldowns, 429 tous-cooldowns, anti-spam token bucket, SSE, cache, rotation de clés prouvée côté spawn, timeout backend, keep-alive, 413, persistance de l'état sur reboot, 12 requêtes concurrentes.

### Modifié
- Cœur serveur extrait dans `src/server.rs` — `ServerConfig` injectable (port éphémère, chemins, exe backend, timings) ; le binaire n'est plus qu'un lanceur de la config prod.

## [0.3.2] — 2026-10-16

### Corrigé
- **SSE streaming** : le client OpenAI d'Hermes streame toujours (`stream: true`) — le router répondait en JSON pur, d'où `EmptyStreamError` (200, 0 chunks) × 3 puis fallback provider à chaque fois. Réponse SSE complète quand `stream: true` (chunks `chat.completion.chunk` → `finish_reason: stop` → `data: [DONE]`), JSON pur sinon.

## [0.3.1] — 2026-10-16

### Corrigé
- **Couche HTTP anti-zombie** : 8 workers fixes (plus aucun thread par requête), timeouts lecture 120 s / écriture 30 s / deadline connexion 600 s, keep-alive HTTP/1.1, chunked encoding, TCP_NODELAY, file bornée 64 avec backpressure, `CALL_TIMEOUT` 80 s par modèle.

## [0.3.0] — 2026-10-16

### Ajouté
- Rewrite Rust (std-only, binaire ~650 KB) : anti-spam token bucket, détection d'exhaustion + cooldowns à backoff exponentiel, cascade de fallback, cache de déduplication, rotation de clés API avec fallback anonyme (jamais de mix), modes `priority`/`roundrobin` à chaud, dashboard `/admin/stats`.
- Workflow GitHub Actions : binaires Windows/Linux/macOS (Intel + Apple Silicon) à chaque tag.

[0.5.1]: https://github.com/DeamonDev888/pauvre-router/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/DeamonDev888/pauvre-router/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.3...v0.4.0
[0.3.3]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/DeamonDev888/pauvre-router/releases/tag/v0.3.0
