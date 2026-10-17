# Router des Pauvres 🗑️🚀

Proxy local OpenAI-compat qui route vers les **modèles frontière gratuits** d'OpenCode Zen — pour ceux qui n'ont pas de budget API. Le « router des pauvres ».

## Pourquoi

Depuis le 14 oct 2026, Zen exige le fingerprint du vrai client OpenCode (TLS+wire vérifié par MITM/replay). Impossible d'appeler l'API en direct avec un simple `curl`. Ce router délègue chaque requête au CLI officiel `opencode run` (qui passe la gate) et expose le tout en format `chat.completions` standard.

## Fonctionnalités

- **OpenAI-compat** : `POST /v1/chat/completions`, `GET /v1/models` — branche n'importe quel client (Hermes, scripts, curl…)
- **⭐ GLM 5.3 Flash gratuit** en tête de cascade (frontière, testé en live)
- **`frontier-robin-free`** — notre pseudo-modèle **round-robin permanent** : chaque requête tourne sur les frontières (glm → space-bunny → deepseek → glm → …). La répartition de charge en un seul nom de modèle.
- **2 modes de routage** (switch à chaud via `/admin/mode`) :
  - `priority` (défaut) : **exhaust le meilleur modèle** avant de basculer sur le suivant
  - `roundrobin` : répartit la charge à chaque requête
- **🔑 Rotation de clés API toujours active** : le pool tourne en round-robin par défaut — clé quota-blocked = cooldown 30 min persistant, la suivante prend le relais, **fallback anonyme** si aucune clé utilisable (jamais de mix clé/anonyme, sinon `Invalid credential` chez Zen)
- **Détection d'exhaustion** : classification (quota/rate-limit/model invalide/timeout) + **cooldowns à backoff exponentiel** (2 min → 1 h)
- **Anti-spam** : token bucket par IP (rafale 5, 20 req/min), plafond 3 spawns opencode, body max 1,5 Mo
- **Cache de déduplication** : prompts identiques servis du cache (TTL 10 min, LRU 128, sha256 maison)
- **Admin** : `/admin/stats`, `/admin/mode`, `/admin/keys`, `/admin/reset`
- **Persistance** : santé + stats (60 s) et cooldowns de clés sur disque
- **Rust std-only** : binaire unique ~650 KB, zéro tokio/reqwest

## Modèles frontière (les meilleurs gratuits, testés en live 2026-10-16)

| Modèle | Latence | Note |
|---|---|---|
| `space-bunny-free` | **6,0 s** | le sprinter |
| `deepseek-v4.1-flash-free` | **6,0 s** | ex æquo, DeepSeek V4.1 |
| ⭐ `glm-5.3-flash-free` | 11,3 s | **frontière, désormais gratuit** — qualité > latence |

Ordre de cascade : `glm-5.3-flash-free` → `space-bunny-free` → `deepseek-v4.1-flash-free`. Le modèle demandé passe toujours en premier ; `frontier-robin-free` route sur les trois en tournant.

## Endpoints

| Endpoint | Description |
|---|---|
| `GET /health` | vivant + version + in-flight |
| `GET /v1/models` | `frontier-robin-free` + les 3 frontières |
| `POST /v1/chat/completions` | chat (cascade/mode automatique) |
| `GET /admin/stats` | usage, santé, spam, pool de clés, mode |
| `GET`/`POST /admin/mode` | mode de routage (`priority`/`roundrobin`) |
| `GET /admin/keys` | méta du pool (jamais les valeurs) |
| `POST /admin/keys/add` | `{"key": "sk-..."}` — ajoute au pool |
| `POST /admin/keys/remove` | `{"key": "sk-..."}` — retire |
| `POST /admin/keys/reanimate` | `{}` ou `{"key": "sk-..."}` — réactive |
| `POST /admin/reset` | purge cache + cooldowns modèles |

## Installation

Guide complet (Windows/Linux/macOS, auto-start, plugin Hermes) : **[SKILL.md](SKILL.md)**. Feuille de route : [TODO.md](TODO.md).

```bash
cargo build --release
./target/release/pauvre-router.exe
```

| Var | Défaut | Rôle |
|---|---|---|
| `PAUVRE_OPENCODE` | chemin auto (Windows) | binaire CLI opencode (obligatoire sur Linux/macOS) |
| `PAUVRE_STATE` | `~/.pauvre-router-state.json` | état (stats/santé) |
| `PAUVRE_KEYS` | `~/.pauvre-router-keys.json` | pool de clés |
| `PAUVRE_MODE` | `priority` | mode de routage au boot |

## Intégration Hermes

```yaml
custom_providers:
- name: zen-free
  base_url: http://127.0.0.1:4445/v1
  api_key: public
  api_mode: chat_completions
  models:
    frontier-robin-free: {}
    glm-5.3-flash-free: {}
    space-bunny-free: {}
    deepseek-v4.1-flash-free: {}
```

⚠️ La liste `models:` du `config.yaml` est **statique** : c'est elle que lit le picker de modèles d'Hermes. Nouveau modèle dans le router → l'ajouter aussi dans le config.

## Plugin desktop Hermes

Pane de monitoring live (santé, cooldowns, stats, mode en badge) — voir [`SKILL.md`](SKILL.md) §3 pour l'installation pas-à-pas.

## Releases

Les binaires Windows/Linux/macOS sont construits par GitHub Actions à chaque tag : page [Releases](../../releases).

## Licence

MIT — mais le nom reste « le router des pauvres ». 🗑️
