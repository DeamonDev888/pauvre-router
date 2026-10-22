# Changelog

Toutes les évolutions notables de **Le Router des Pauvres**.
Format basé sur [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) — sémantique [SemVer](https://semver.org/).

## [0.9.0] — 2026-10-22

### Corrigé
- **LE JOURNAL DES OUTILS ÉTAIT SERVI AU CLIENT (bug « desktop »)** :
  en mode agent (`--agent build`), `opencode run` écrit la réponse sur
  stdout et le **journal des appels d'outils** (`$ echo … && ls src` +
  la sortie de la commande) sur stderr. Le routeur fusionnait les deux
  (v0.8.3) → le client Hermes desktop recevait le journal au lieu de la
  réponse (`HELLO_FROM_AGENT\n\nExécution réussie…`), et la bannière
  pouvait réapparaître en fin de réponse (pansé en v0.8.8 par
  `strip_banner_lines`).
  **Correctif à la racine** : le spawn passe désormais `--format json`
  → opencode émet une **ligne JSON par événement** ; le nouveau module
  `src/ocjson.rs` ne garde que les événements `type == "text"` (la
  réponse) et jette les `tool_use` (le journal). `strip_banner` /
  `strip_banner_lines` sont SUPPRIMÉS : ils ne faisaient que masquer le
  symptôme. Au passage, les erreurs amont deviennent lisibles par le
  classifieur au lieu d'être noyées dans 28 Ko de transcript.
- **« Unexpected server error » ne cloue plus un modèle valide pendant
  1 h** : c'est une panne générique de la gateway opencode, pas un
  modèle inconnu. Le classifieur la classe `Transient` (cooldown court)
  **avant** le bucket « modèle invalide » (le message contient « not
  available » dans certaines variantes). Symptôme vu en prod :
  `minimax-m3.1-flash-free` et `longcat-2.5-preview-free` en cooldown
  3600 s alors que la cascade n'avait qu'un modèle survivant.
- **DEADLINE GLOBALE DE LA CASCADE** : sans elle, une cascade de
  7 modèles × 240 s = **28 minutes de silence** avant la première
  réponse — le client desktop abandonnait son tour (« ça ne répond
  pas »). Le budget total est maintenant borné (2× le timeout en mode
  agent, 3× en mode texte) et le timeout effectif d'une tentative ne
  peut pas dépasser le reste du budget. Au dépassement : erreur
  lisible (502 « delai global de la cascade depasse »).
- **L'exploration du robin n'explorait PLUS RIEN** : l'index était
  `cursor % len` calculé APRÈS incrément — avec 5 modèles, 5 % 5 == 0
  retombait toujours sur le premier, celui que l'exploitation choisit
  (tie-break). Observé : 9 requêtes, 9× le même modèle, les autres
  jamais mesurés. L'index porte maintenant sur le nombre
  d'explorations et saute le modèle d'exploitation.

### Modèles
- Rotation du 22 oct 2026 (test de liveness réel sur chaque modèle) :
  `longcat-2.5-preview-free`, `minimax-m3.1-flash-free`,
  `flint-40b-free` et `jev-1.13-free` sont **morts** (erreur gateway
  « Unexpected server error » / « Endpoint is unavailable ») ;
  `beam-free` est **instable** (timeout récurrent > 90 s).
  Nouvelle chaîne : `big-pickle` → `glm-5.3-flash-free` →
  `nemotron-3.5-lightning-free` → `mimo-v2.6-flash-free` →
  `deepseek-v4.1-flash-free`.

### Testé
- Les tests E2E ne codent plus **en dur** de noms de modèles (la
  rotation les cassait) : ils dérivent la liste de `FREE_MODELS`.
- Nouveaux tests de régression : le journal d'outils ne fuit ni en JSON
  ni en SSE, l'erreur structurée atteint le classifieur, la deadline
  globale coupe la cascade, l'exploration touche un autre modèle.

## [0.8.8] — 2026-10-22

### Corrigé
- **Bannière qui fuyait dans les réponses** : depuis la fusion
  stdout+stderr (v0.8.3), la bannière `> plan · <modèle>` (stderr) n'est
  plus en tête de la transcription — `strip_banner` la rate et elle
  s'ajoutait À LA FIN des réponses (« ROUTEUR-OK\n\n> plan · beam-free »).
  Nouveau `strip_banner_lines` : filtre TOUTES les lignes-bannières
  `> build|plan · <modèle>` où qu'elles soient.

### Ajouté
- **`GET /admin/logs?x-tail=N`** : queue du buffer mémoire de logs
  (500 lignes max, tronquées à 500 chars) — l'agent backend peut se
  diagnostiquer SANS accès disque (il cherchait le fichier de log hors
  de sa sandbox et prenait un 404). Les erreurs backend (`model=…
  kind=… cd=…`) alimentent le buffer en plus de stderr.

## [0.8.7] — 2026-10-21

(publiée avec les changements 0.8.6 ci-dessous, trouvés par l'auto-audit
continu de l'agent backend)

## [0.8.6] — 2026-10-21

### Corrigé
- **Le flux SSE ne peut plus recevoir une 2e réponse HTTP** : à l'ouverture
  du flux (`data:` rôle), la réponse HTTP est DÉJÀ partie ; en cas
  d'échec de la cascade entière, le serveur y écrivait une réponse HTTP
  brute (`502 Bad Gateway`) — le chunked en cours était CORROMPU (2e
  status line dans le flux) et le client OpenAI cassait en « Connection
  error » au lieu de lire l'erreur. L'erreur part désormais en payload
  `data: {"error":{…}}` puis `[DONE]` + clôture propre du flux (même
  forme que le chemin « tous les modèles en cooldown »).

### Robustesse
- **Le service ne peut plus devenir muet sous keep-alive** : un worker
  sert sa connexion jusqu'à sa mort (il attend la requête suivante
  pendant `MAX_IDLE`), et une requête chat longue le retient jusqu'à
  4 min. Avec 8 workers, 8 connexions keep-alive INACTIVES (le cas
  normal d'un SDK OpenAI qui garde son pool de sockets) suffisaient à
  immobiliser tout le pool : plus un seul `/health` ni chat servi. Le
  pool passe à **24 workers** — la pression amont reste bornée par
  ailleurs (token-bucket par IP, budget anti-ban, gate backend à 3) :
  plus de connexions servies, jamais plus d'appels amont.

## [0.8.5] — 2026-10-21

### Corrigé
- **Les watchdogs clients comptent les `data:`, pas les commentaires** :
  le keepalive `: keepalive N` (commentaire SSE) n'apaise PAS les
  watchdogs OpenAI SDK (« 60 s waiting for the first stream chunk ») —
  ils attendent un VRAI événement `data:`. Le flux envoie maintenant :
  1. le **rôle initial** (`data:` delta rôle) dès l'ouverture (<100 ms) ;
  2. un **delta vide porteur** (`data:` finish:null) toutes les 5 s
     pendant tout le travail backend (audit réel = 2-10 min) ;
  3. le corps de la réponse (chunks ~240 chars) puis [DONE] à la
     livraison — SANS réémettre le rôle, avec le même stream id.
  Un audit long tient donc dans UN stream vivant du point de vue du
  client, au lieu d'un flux muet que le watchdog tue à 60 s.

## [0.8.4] — 2026-10-21

### Corrigé
- **Workdir du client écrasait l'auto-référence** : « audit du router des
  pauvre » passait le cwd du CLIENT comme répertoire de travail — l'agent
  backend auditait `mcp-command-center` (fichiers ui-e2e, commit
  « fix(ui) v0.6.5 ») au lieu du service. Découverte du repo du service
  au boot (`discover_self_repo` : remontée depuis current_exe vers le
  Cargo.toml « pauvre-router ») : si la DERNIÈRE question user mentionne
  « pauvre » / « pauvre-router », le workdir du service ÉCRASE celui du
  client. Hors auto-référence, le workdir client reste respecté.

## [0.8.3] — 2026-10-21

### Corrigé
- **Auto-empoisonnement classifieur** : en mode agent, opencode sort la
  RÉPONSE ET la transcription des outils sur STDERR (28 Ko vus) — et un
  audit du routeur qui lit `src/classify.rs`/`CHANGELOG.md` contient
  fatalement les chaînes « abusive traffic » que le classifieur cherche !
  L'audit DU ban recréait l'état de ban (circuit ouvert, cascade brûlée).
  Fixes : (1) stdout+stderr traités comme UNE transcription en mode agent
  (la réponse réelle est souvent sur stderr), (2) strip ANSI avant
  strip_banner (bannières à codes couleurs non reconnues), (3) seulement
  ~400 premiers chars de la transcription passés au classifieur (les
  erreurs amont sont toujours EN TÊTE, jamais après 28 Ko d'outils).

## [0.8.2] — 2026-10-21

### Corrigé
- **Flux SSE refermé au premier ping** : `write_stream` émet le chunk
  terminal HTTP dès son callback — le préambule du keepalive clôturait
  donc la réponse immédiatement (client : flux « fini » à 0,03 s, corps
  jamais reçu ; serveur : travail terminé en arrière-plan, mis au cache,
  jamais livré). Nouveau `stream_open` (headers + contenu SANS chunk
  terminal) + `stream_close` à la livraison ; `write_stream` inchangé
  pour les réponses d'un bloc.
- Diagnostic « backend success VIDE » ajouté (sortie bannière-seule).

## [0.8.1] — 2026-10-21

### Ajouté
- **Keepalive SSE pendant le travail backend** : l'appel opencode part sur
  un thread dédié et le flux SSE est ouvert IMMÉDIATEMENT (ping 0), puis
  un commentaire `: keepalive N` part toutes les 5 s jusqu'à la fin du
  travail (limité par le timeout backend, pas par le client). Fini le
  « 60 s waiting for the first stream chunk » des watchdogs clients :
  les headers et le premier octet partent en < 100 ms, le client reçoit
  du trafic en continu pendant qu'un audit réel (2-10 min) s'exécute.
  Client parti en cours de route : le ping s'arrête mais le travail se
  TERMINE et va au cache (requête identique suivante servie instantanément).
- Erreurs en flux ouvert : les 429 « tous en cooldown » partent en payload
  `data:` + [DONE] (jamais de HTTP brut après des headers SSE).

### Corrigé
- Elapsed backend calculé via `saturating_duration_since` (anti-panic,
  trouvé par l'audit de l'agent backend lui-même).

## [0.8.0] — 2026-10-21

### Corrigé
- **Confusion d'identité du backend agent** (audit 21 oct) : la demande
  « audit du router des pauvre » passée à travers le proxy parlait à un
  backend qui ne sait pas QU'IL EST le backend du Router des Pauvres —
  piégé par le persona cybersécurité du client, il est parti auditer le
  routeur LAN (SSH 192.168.40.1, scan passerelle) au lieu du service
  lui-même, et a brûlé la cascade sur des lectures hors workdir
  (`external_directory … auto-rejecting`). Règle IDENTITÉ ajoutée au mode
  agent : « tu es le backend du service pauvre-router (127.0.0.1:4445,
  repo dans ton workdir) ; toute mention du routeur désigne CE service,
  jamais un routeur réseau — zéro scan LAN, zéro SSH » + consigne de
  continuation après rejet sandbox (ne jamais abandonner la tâche).

## [0.7.9] — 2026-10-20

### Corrigé
- **PATH amputé en mode agent** : `backend.rs` forçait `PATH=C:\Windows\System32`
  sur TOUS les spawns — l'agent build exécutait ses outils bash avec ce PATH
  nu → `ls: command not found`, `git`/`cargo` introuvables sur chaque appel
  outil (vu en prod pendant un audit réel). En mode AGENT, le spawn reçoit
  maintenant un PATH complet (git-bash usr/bin + mingw64 + cmd + système +
  PATH du routeur en secours) ; le mode TEXTE garde le minimal.

## [0.7.8] — 2026-10-20

### Corrigé
- **Pollution du backend par le system prompt du client** (audit 20 oct) :
  Hermes transmet son system prompt ENTIER (listes de skills, directives
  mémoire, marqueurs de steering — 15-40k chars). Le modèle backend
  obéissait à ces instructions et tentait d'invoquer des mécanismes
  inexistants dans opencode → `✗ Skill "pauvre-router" failed — Error:
  Skill not found` sur CHAQUE appel (3 échecs minimax, 4 deepseek vus en
  prod), brûlant la cascade entière — d'où les « 60 s sans premier chunk »
  côté client. Deux garde-fous :
  - **`MAX_SYS_CHARS` 6000** : la consigne système est tronquée (début
    conservé = persona + instructions principales, marqueur de troncature
    ajouté) — le bloc de skills du client ne transite plus ;
  - **Règle FRONTIÈRE D'ENVIRONNEMENT** (mode tools) : le backend est
    informé que la consigne vient d'un AUTRE agent et que tout ce qui
    n'est pas dans SES outils (skills, skill_view, memory, cron,
    sous-agents…) n'existe pas ici — n'invoquer JAMAIS ces mécanismes.
- +2 tests (99 total).

## [0.7.7] — 2026-10-20

### Corrigé
- **413 qui devenait « Connection error »** : un corps dépassant la limite
  recevait un 413 sans que le serveur lise le body — les octets restants
  frappaient un socket fermé → RST → le client (SDK OpenAI/Hermes) voyait
  une `APIConnectionError` au lieu du vrai diagnostic. Le serveur draine
  maintenant le corps (borne 64 Mo, timeout de drain 2 s) AVANT de répondre
  413 proprement.
- **`MAX_BODY` 1,5 Mo → 8 Mo** : une session Hermes de ~850 messages
  (~1,2-1,5 Mo de JSON) frappait l'ancienne limite de façon intermittente —
  c'était la cause des « Connection error » instantanés (23 ms) suivis d'un
  fallback forcé sur un autre provider.
- +2 tests E2E : session de 850 messages (≈2 Mo) qui passe entière, 413
  propre sur 9 Mo sans RST (97 tests total).

## [0.7.6] — 2026-10-19

### Changé
- **Cascade frontier mise à jour** (inventaire live de la gateway Zen : 86 modèles) :
  - **`space-bunny-free` retiré** — disparu de la gateway, `UnknownError`
    systématique (`jev-1.13-free`, apparu en même temps, est cassé aussi) ;
  - **3 nouveaux free ajoutés et testés en direct** : `nemotron-3.5-lightning-free`
    (~5 s), `longcat-2.5-preview-free` (~7 s), `mimo-v2.6-flash-free` (~7 s),
    puis `minimax-m3.1-flash-free` (~8 s) sur demande de l'opérateur ;
  - la cascade passe de 4 à 7 frontières : plus de redondance, même budget
    anti-ban (chaque requête n'en frappe qu'une avant de réussir).
- +3 modèles exposés dans `/v1/models` (robin + 7).

## [0.7.5] — 2026-10-19

### Ajouté
- **Module anti-ban** (`src/antiban.rs`) — la leçon de l'incident du 18 oct 2026
  (IPs VPN ET maison flaggées « abusive traffic » ~24 h par la gateway Zen après
  ~800 messages/jour en anonyme) :
  - **Budget global d'appels backend** : 30/min par défaut (env `PAUVRE_BUDGET`,
    plancher 6, fenêtre glissante 60 s) — au-delà, 429 immédiat au client,
    **aucun spawn** opencode. L'anti-spam existant bornait par IP cliente ;
    rien ne bornait le flux TOTAL vers l'amont.
  - **Circuit breaker flag IP** : la bannière « free models aren't available /
    abusive traffic » (nouveau `ErrorKind::IpFlagged`, classée AVANT
    ModelInvalid qui matchait « not available ») ouvre un circuit 10 min —
    tout appel backend refusé sans spawn (marteler une gateway qui a flaggé
    l'IP ne fait qu'aggraver le flag). Un `retry after N` amont prolonge la
    coupure (×2, plafonné) ; un succès backend referme le circuit.
  - **`GET /admin/antiban`** : budget, appels de la fenêtre, flag, secondes
    de circuit restantes (aussi embarqué dans `/admin/stats`).
  - **`POST /admin/antiban`** : `{"budget":N}` à chaud (plancher 6),
    `{"release":true}` — levier opérateur après changement de réseau
    (VPN off, autre IP) pour sortir du circuit sans attendre la sonde.
  - Cooldown modèle `IpFlagged` court (90 s, sans backoff) : l'arrêt global
    est porté par le circuit, la cascade n'empile plus 4 backoff géants.
  - Endpoint admin `POST /admin/antiban` : changement de budget à chaud.
- +9 tests (95 total) : budget/fenêtre, plancher, flag/circuit, prolongement
  plafonné, ban IP vs ModelInvalid, extraction `retry after`, 3 E2E (budget
  sans spawn, circuit ouvert/refermé, endpoint admin).

## [0.7.0] — 2026-10-17

### Ajouté
- **Robin intelligent** (`pick_robin_smart`) : le round-robin aveugle devient un sélecteur par score —
  - **EWMA de latence** par modèle (alpha 1/4) : lisse les pics, réagit en ~4 requêtes ;
  - **score** = EWMA + 30 s/échec consécutif + 5 s/dépassement de 20 s — un modèle dégradé est évité dès la requête suivante ;
  - **exploration 1/5** : une requête sur cinq part sur le modèle suivant pour rafraîchir les mesures (un modèle redevenu rapide est redétecté en ≤ 5 requêtes) ;
  - **timeout adaptatif** par modèle (1,5× EWMA, borné [30 s, base]) — un modèle lent connu a une vraie marge, un rapide est coupé tôt pour déclencher le fallback vite ;
  - `/admin/stats` expose désormais `ewma_latency_ms` et `slow_count` par modèle.
- **SSE multi-chunks** : les réponses sont découpées en chunks ~240 chars (frontières ligne/espace, UTF-8 safe) — les clients rendent progressivement au lieu d'attendre le bloc entier.
- +11 tests (84 total) : EWMA, score, exploitation/exploration, timeout adaptatif, découpe SSE (UTF-8 inclus).

## [0.6.1] — 2026-10-17

### Corrigé
- **Updater brické après le premier échec réseau** : le flag « check en cours » n'était pas réarmé quand l'appel GitHub échouait (réseau pas prêt au boot) — tous les checks suivants étaient ignorés à jamais (`checked_at_epoch` restait 0, vu en prod). Le flag est maintenant réarmé dans les deux branches ; prouvé live : check au boot OK, `latest: 0.6.0` détecté, `update_available: false` sur un router à jour.

## [0.6.0] — 2026-10-17

### Ajouté
- **Module de détection de mise à jour** (`src/update.rs`) :
  - check GitHub Releases au boot puis toutes les heures (curl 6 s max, jamais bloquant, ne panic jamais) ;
  - comparaison semver maison (tags non conformes ignorés) ;
  - **`GET /admin/update`** : dernier état connu (current / latest / update_available / url) ;
  - **`POST /admin/update/check`** : check forcé à la demande (répond toujours 200, même réseau KO) ;
  - **`GET /health` et `/admin/stats`** exposent maintenant un bloc `update` — n'importe quel client (plugin desktop, scripts) voit `update.available: true` et l'URL de téléchargement dès qu'une release plus récente existe. Le téléchargement reste un choix de l'opérateur : le router signale, ne s'auto-remplace pas.

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

[0.7.0]: https://github.com/DeamonDev888/pauvre-router/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/DeamonDev888/pauvre-router/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/DeamonDev888/pauvre-router/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/DeamonDev888/pauvre-router/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/DeamonDev888/pauvre-router/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.3...v0.4.0
[0.3.3]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/DeamonDev888/pauvre-router/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/DeamonDev888/pauvre-router/releases/tag/v0.3.0
