# Journal d'audit — pauvre-router v0.7.4

**Date :** dimanche 18 octobre 2026, ~10:55–11:15 (UTC-04:00)
**Audit demandé :** « le routeur des pauvres ne fonctionne pas »
**Méthode :** tests réels sur le service live (127.0.0.1:4445) + code review.
**Champ d'application :** machine Windows locale uniquement.

## 1. Verdict

**Le code du routeur est sain ; l'IP de la machine est flaggée « abusive »
par OpenCode — cause : volume d'usage automatisé, pas le VPN.**

Message exact de la gateway (`opencode.ai/zen/v1`, via le CLI) :
> « OpenCode's free models aren't available from your current network
> because we've detected abusive traffic from this IP address. »

**Corrections d'hypothèses pendant l'audit (méthode éliminatoire) :**
1. ~~IP VPN bannie~~ — FAUX : après déconnexion VPN, IP maison
   (Cogeco résidentiel, 157.208.25.18, non-proxy/non-hosting) bannie aussi.
2. ~~Fingerprint de l'install opencode~~ — FAUX : profil de données frais
   (USERPROFILE vierge, nouveau opencode.db) → même ban.
3. ~~Trafic automatisé en cours pendant les tests~~ — FAUX : 0 process
   opencode, compteur du routeur figé (233 req sur 45 s d'observation).
4. **Retenu : flag persistant sur l'IP après franchissement de seuil de
   volume.** Preuves : ban confirmé 35+ min après l'arrêt de tout trafic ;
   le GET `/zen/v1/models` (léger) passe en 200, seul l'endpoint
   d'inférence est filtré ; le curl brut reçoit une AUTRE 403
   (`FreeTierError` gate fingerprint — normale), le ban ne vise que le CLI.

## 2. Ce qui a été vérifié (tout vert)

| Vérification                          | Résultat                                   |
|---------------------------------------|--------------------------------------------|
| Binaire en mémoire vs HEAD            | identiques (v0.7.4, 4f791c3)               |
| `cargo test`                          | 86 tests verts (61 lib + 6 stub + 19 e2e)  |
| `cargo clippy -D warnings`            | OK (après fix trivial `tests/e2e.rs:403`)  |
| `cargo fmt --check`                   | OK                                         |
| `/v1/models` (5 modèles annoncés)     | OK                                         |
| `/health`, `/admin/stats`             | OK, JSON propres                           |
| Endpoints admin (auth Bearer)         | protégés                                   |
| Cascade frontier / cooldown           | fonctionne comme prévu (4 fallbacks)       |

## 3. Chronologie des preuves

- 10:36 — process `pauvre-router.exe` PID 5384 démarré ; **120 appels OK**
  en ~20 min (stats `/admin/stats`) avant la première erreur à ~10:55.
- ~10:55 — 5× `POST /v1/chat/completions` sur les 5 modèles : toutes
  échouent (`upstream_error` / cooldown global après cascade).
- Test direct d'opencode hors routeur : même message de bannissement IP.
- 10:57 — `https://api.ipify.org` → `195.242.214.19` = sortie VPN ;
  `ssh kali` → `Permission denied` (VPN capture le LAN).
- VPN déconnecté (~11:20) — nouvelle IP `157.208.25.18` (Cogeco
  résidentiel, proxy=false, hosting=false) : **CLI toujours banni**.
- Table de routage vérifiée : route par défaut = 192.168.40.1 (box),
  plus aucun adaptateur VPN, pas de proxy env, pas d'entrée hosts.
- curl brut sur `opencode.ai/zen/v1/chat/completions` : 403 avec un
  message DIFFÉRENT (`FreeTierError` — gate fingerprint, attendue hors
  CLI) ; `GET /zen/v1/models` : 200 OK → filtrage ciblé sur l'inférence.
- `opencode auth list` : 0 credentials (usage 100% anonyme confirmé).
- Profil opencode frais (USERPROFILE vierge) : même ban → pas un flag
  d'empreinte locale.
- 11:35+ — zéro trafic en cours (0 process, compteur routeur figé) et le
  ban persiste → flag IP durable, pas une limite tempo active.
- **Volume DB opencode** : 370 sessions / 1 115 messages au total ;
  oct-17 : **814 messages, 239 sessions** ; oct-18 matin : 40 messages /
  20 sessions avant le flag. ~1 400 tokens input/message en moyenne.
- Test clippy réel : 1 erreur `unused variable vc` (tests/e2e.rs:403) →
  corrigée (`_vc`) → clippy + fmt verts.

## 4. Cause racine (chaîne complète)

1. Le routeur spawn `opencode.exe` (anonyme, 0 credentials) pour chaque
   requête — plusieurs spawns par requête client en cascade.
2. Volume automatisé massif : 814 messages hier, 120 appels OK ce matin
   → franchissement du seuil « abusive traffic » d'OpenCode en milieu de
   matinée (jamais de 429/Exhausted vu avant : le passage au ban est sec).
3. La gateway `opencode.ai/zen/v1` flague l'IP (durable, pas un cooldown)
   et refuse les modèles free au CLI ; la détection est indépendante du
   compte (0 credentials) et de l'empreinte locale (profil frais testé).
4. Le routeur classifie `Transient` (correct), cascade les 4 modèles
   frontier — tous frappés par le même ban IP → cooldown global 30 s.
5. Le VPN n'était qu'un facteur aggravant (2 IPs différentes bannies :
   195.242.214.19 puis 157.208.25.18).

## 5. Correctifs appliqués

- `tests/e2e.rs:403` : `vc` → `_vc` (var inutilisée, clippy -D warnings).
- Commit `80d785e` poussé sur main.

## 6. Recommandations

### Résoudre le blocage actuel
1. **Attendre la levée du flag** (durée inconnue, heures à jours) avec
   zéro trafic — tout nouvel essai depuis cette IP le ré-arme peut-être.
2. **Réduire le volume** ensuite : le free tier anonyme n'est pas conçu
   pour 800+ messages/jour automatisés. Options : limiter le routeur
   (rate-limit global / cache déjà présent : 7 hits), réserver le free
   pour de petits prompts, ou `opencode auth login` (compte payant
   OpenCode Go contourne le free tier et son anti-abus).
3. **Autre réseau** pour tester la levée : partage de connexion mobile
   (IP différente) → si le CLI passe, ban IP confirmé à 100%.

### Robustesse routeur (optionnel, code)
4. Classifier le message « aren't available from your current network »
   comme cas dédié (`IpFlagged`) avec réponse claire au client au lieu
   d'un `Transient` générique + cascade inutile (les 4 modèles partagent
   la même gateway → cascade vouée à l'échec quand c'est un ban IP).
5. Exposer la sortie du flag (détection du message) dans `/admin/stats`
   pour diagnostic immédiat.

### Kali (incident séparé, constaté pendant l'audit)
6. `192.168.40.15` : ping 1/3 OK, port 22 timeout → probable WiFi
   instable ou machine endormie ; à vérifier quand le réseau sera
   propre (télécommande impossible tant que SSH est down).
