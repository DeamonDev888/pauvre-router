# Router des Pauvres 🗑️🚀

Proxy local OpenAI-compat qui route vers les **modèles gratuits frontière** d'OpenCode Zen — pour ceux qui n'ont pas de budget API. Le « router des pauvres ».

## Pourquoi

Depuis le 14 oct 2026, Zen exige le fingerprint du vrai client OpenCode (TLS+wire vérifié par MITM/replay). Impossible d'appeler l'API en direct avec un simple `curl`. Ce router délègue chaque requête au CLI officiel `opencode run` (qui passe la gate) et expose le tout en format `chat.completions` standard.

## Fonctionnalités

- **OpenAI-compat** : `POST /v1/chat/completions`, `GET /v1/models` — branche n'importe quel client (Hermes, scripts, etc.)
- **Cascade de fallback** : modèle demandé en premier, puis bascule automatique sur les autres modèles frontière gratuits si exhaustion
- **Détection d'exhaustion** : classification des erreurs (quota/rate-limit/model invalide/timeout) avec **cooldowns à backoff exponentiel** par modèle
- **Anti-spam** : token bucket par IP (rafale 5, 20 req/min) + plafond de concurrence (3 spawns opencode max) + limite de body 1,5 Mo
- **Cache de déduplication** : prompts identiques → réponse servie du cache (TTL 10 min, LRU 128 entrées, clé sha256 maison)
- **Endpoint admin** : `/admin/stats` (usage par modèle, santé, cooldowns restants, spam récent), `/admin/reset`
- **Persistance** : état de santé + stats sauvés toutes les 60 s dans `~/.pauvre-router-state.json`, rechargés au démarrage
- **Rust std-only** : zéro tokio, zéro reqwest — compile en ~30 s, binaire unique ~2 Mo

## Modèles (cascade par défaut)

`space-bunny-free` → `mimo-v2.6-flash-free` → `nemotron-3.5-lightning-free` → `longcat-2.5-preview-free`

## Endpoints

| Endpoint | Description |
|---|---|
| `GET /health` | vivant + version + in-flight |
| `GET /v1/models` | liste des modèles gratuits |
| `POST /v1/chat/completions` | chat (cascade automatique) |
| `GET /admin/stats` | usage, santé, spam récent |
| `POST /admin/reset` | purge cache + cooldowns |
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
| `PAUVRE_STATE` | `~/.pauvre-router-state.json` | fichier d'état |

## Test rapide

```bash
curl -s http://127.0.0.1:4445/health
curl -s -X POST http://127.0.0.1:4445/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"space-bunny-free","messages":[{"role":"user","content":"PING ?"}]}'
```

## Intégration Hermes

```yaml
custom_providers:
- name: zen-free
  base_url: http://127.0.0.1:4445/v1
  api_key: public
  api_mode: chat_completions
  models:
    space-bunny-free: {}
    mimo-v2.6-flash-free: {}
```

## Plugin desktop Hermes

Voir [`plugin/`](plugin/) — un plugin `pauvre-router` pour le desktop Hermes : pane de monitoring (santé des modèles, cooldowns, stats live). Copier `plugin/` dans `$HERMES_HOME/desktop-plugins/pauvre-router/`.

## Licence

MIT — mais le nom reste « le router des pauvres ». 🗑️
