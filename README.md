# Router des Pauvres 🗑️🚀

Proxy local OpenAI-compat qui route vers les **modèles gratuits frontière** d'OpenCode Zen — pour ceux qui n'ont pas de budget API. Le « router des pauvres ».

## Pourquoi

Depuis le 14 oct 2026, Zen exige le fingerprint du vrai client OpenCode (TLS+wire vérifié par MITM/replay). Impossible d'appeler l'API en direct avec un simple `curl`. Ce router délègue chaque requête au CLI officiel `opencode run` (qui passe la gate) et expose le tout en format `chat.completions` standard.

## Fonctionnalités

- **OpenAI-compat** : `POST /v1/chat/completions`, `GET /v1/models` — branche n'importe quel client (Hermes, scripts, curl…)
- **⭐ GLM 5.3 Flash gratuit** en tête de cascade (frontière, testé en live)
- **2 modes de routage** (switch à chaud via `/admin/mode`) :
  - `priority` (défaut) : **exhaust le meilleur modèle** (cooldown = exhaustion) avant de basculer sur le suivant
  - `roundrobin` : **répartit la charge** — chaque requête part sur le modèle suivant de la cascade
- **Détection d'exhaustion** : classification des erreurs (quota/rate-limit/model invalide/timeout) avec **cooldowns à backoff exponentiel** par modèle (2 min → 1 h)
- **Pool de clés API Zen** (rotation anti-quota) : `POST /admin/keys/add` — round-robin entre les clés, une clé quota-blocked est mise en cooldown 30 min (persistant) et la rotation passe à la suivante ; **fallback anonyme** si aucune clé utilisable (les free passent sans clé — jamais de mix clé/anonyme, sinon `Invalid credential`)
- **Anti-spam** : token bucket par IP (rafale 5, 20 req/min) + plafond de concurrence (3 spawns opencode max) + limite de body 1,5 Mo
- **Cache de déduplication** : prompts identiques → réponse servie du cache (TTL 10 min, LRU 128 entrées, clé sha256 maison)
- **Endpoint admin** : `/admin/stats` (usage par modèle, santé, cooldowns restants, spam récent, état du pool de clés), `/admin/reset`, `/admin/mode`, `/admin/keys`
- **Persistance** : état de santé + stats (60 s) et cooldowns de clés dans `~/.pauvre-router-state.json` / `~/.pauvre-router-keys.json`
- **Rust std-only** : zéro tokio, zéro reqwest — binaire unique ~650 KB

## Modèles gratuits — classés par latence (bench live 2026-10-16, spawn froid inclus)

| # | Modèle | Latence | Note |
|---|---|---|---|
| 🥇 | `space-bunny-free` | **6,0 s** | le sprinter — cascade par défaut |
| 🥇 | `deepseek-v4.1-flash-free` | **6,0 s** | ex æquo, DeepSeek V4.1 |
| 🥉 | `longcat-2.5-preview-free` | 8,4 s | preview LongCat |
| 4 | `mimo-v2.6-flash-free` | 9,8 s | Xiaomi MiMo |
| ⭐ | `glm-5.3-flash-free` | 11,3 s | **GLM 5.3 Flash — frontière, désormais gratuit** (qualité > latence) |
| 🐌 | `nemotron-3.5-lightning-free` | 29,2 s | Lightning… pas si lightning |

(`nemotron-3-ultra-free` est écarté de la cascade : timeout > 100 s constaté.)

Ordre de cascade : `glm-5.3-flash-free` → `space-bunny-free` → `mimo-v2.6-flash-free` → `deepseek-v4.1-flash-free` → `nemotron-3.5-lightning-free` → `longcat-2.5-preview-free` (le modèle demandé passe toujours en premier, la frontière en tête).

## Endpoints

| Endpoint | Description |
|---|---|
| `GET /health` | vivant + version + in-flight |
| `GET /v1/models` | liste des modèles gratuits |
| `POST /v1/chat/completions` | chat (cascade/mode automatique) |
| `GET /admin/stats` | usage, santé, spam récent, pool de clés, mode |
| `GET /admin/mode` | mode de routage courant |
| `POST /admin/mode` | `{"mode": "priority"\|"roundrobin"}` |
| `GET /admin/keys` | méta du pool (jamais les valeurs) |
| `POST /admin/keys/add` | `{"key": "sk-..."}` — ajoute une clé au pool |
| `POST /admin/keys/remove` | `{"key": "sk-..."}` — retire une clé |
| `POST /admin/keys/reanimate` | `{}` ou `{"key": "sk-..."}` — réactive (purge cooldowns) |
| `POST /admin/reset` | purge cache + cooldowns modèles |
| `OPTIONS *` | CORS preflight |

## Build & run

```bash
cargo build --release
./target/release/pauvre-router.exe
```

Variables d'env :

| Var | Défaut | Rôle |
|---|---|---|
| `PAUVRE_OPENCODE` | chemin opencode.exe | binaire CLI officiel à invoquer |
| `PAUVRE_STATE` | `~/.pauvre-router-state.json` | fichier d'état (stats/santé) |
| `PAUVRE_KEYS` | `~/.pauvre-router-keys.json` | fichier du pool de clés |
| `PAUVRE_MODE` | `priority` | mode de routage au démarrage |

## Test rapide

```bash
curl -s http://127.0.0.1:4445/health
curl -s -X POST http://127.0.0.1:4445/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"glm-5.3-flash-free","messages":[{"role":"user","content":"PING ?"}]}'

# ajouter une clé Zen au pool (rotation anti-quota)
curl -s -X POST http://127.0.0.1:4445/admin/keys/add -d '{"key":"sk-xxxxxxxxxxxxxxxxxxxx"}'
# passer en répartition de charge
curl -s -X POST http://127.0.0.1:4445/admin/mode -d '{"mode":"roundrobin"}'
```

## Intégration Hermes

```yaml
custom_providers:
- name: zen-free
  base_url: http://127.0.0.1:4445/v1
  api_key: public
  api_mode: chat_completions
  models:
    glm-5.3-flash-free: {}
    space-bunny-free: {}
    mimo-v2.6-flash-free: {}
    deepseek-v4.1-flash-free: {}
    nemotron-3.5-lightning-free: {}
    longcat-2.5-preview-free: {}
```

⚠️ La liste `models:` du `config.yaml` est **statique** : c'est elle que lit le picker `/model` d'Hermes. Si tu ajoutes un modèle dans le router, ajoute-le aussi dans le config (sinon il n'apparaît pas).

## Plugin desktop Hermes

Voir [`plugin/`](plugin/) — pane de monitoring live (santé des modèles, cooldowns, stats, mode de routage). Copier `plugin/` dans `$HERMES_HOME/desktop-plugins/pauvre-router/`, puis ⌘K → *Reload desktop plugins*.

## Licence

MIT — mais le nom reste « le router des pauvres ». 🗑️
