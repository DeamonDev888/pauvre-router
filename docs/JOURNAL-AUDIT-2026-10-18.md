# Journal d'audit — pauvre-router v0.7.4

**Date :** dimanche 18 octobre 2026, ~10:55–11:15 (UTC-04:00)
**Audit demandé :** « le routeur des pauvres ne fonctionne pas »
**Méthode :** tests réels sur le service live (127.0.0.1:4445) + code review.
**Champ d'application :** machine Windows locale uniquement.

## 1. Verdict

**Le code du routeur est sain ; le service est victime de son réseau.**
Toutes les erreurs remontées aux clients proviennent d'OpenCode (l'amont)
qui **bannit l'IP de sortie du VPN** du PC :

> « OpenCode's free models aren't available from your current network
> because we've detected abusive traffic from this IP address. »

- IP de sortie actuelle : `195.242.214.19` (IP VPN, pas l'IP maison).
- OpenCode détecte le trafic comme abusif depuis cette IP → refuse les
  modèles gratuits → le routeur classifie `Transient`, cascade sur les
  4 modèles frontier → tous bannis → cooldown global de 30 s.

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

- 10:36 — process `pauvre-router.exe` PID 5384 démarré (depuis
  `target\release`, binaire du commit HEAD).
- ~10:55 — 5× `POST /v1/chat/completions` sur les 5 modèles : toutes
  échouent (`upstream_error` / cooldown global après cascade).
- Test direct d'opencode hors routeur : même bannière de bannissement IP
  (« turn it off and try again ») — cause amont confirmée.
- 10:57 — `https://api.ipify.org` → `195.242.214.19` = sortie VPN.
- 10:58 — `ssh kali` → `Permission denied` (piège VPN documenté dans le
  MEMO serveur Kali) : le VPN capture tout le trafic de la machine.
- Test clippy réel : 1 erreur `unused variable vc` (tests/e2e.rs:403) →
  corrigée (`_vc`) → clippy + fmt verts.

## 4. Cause racine (chaîne complète)

1. Le VPN Windows (pool 10.2.0.x) route TOUT le trafic (0.0.0.0/0).
2. Son IP de sortie `195.242.214.19` est bannie par OpenCode
   (« abusive traffic », indépendant du compte).
3. opencode CLI renvoie l'erreur → routeur classifie `Transient`
   (correct : ni `Exhausted` ni `ModelInvalid`).
4. Cascade frontier : essai des 4 modèles → tous reçoivent le même ban.
5. Cooldown global 30 s → erreur finale `upstream_error`.

## 5. Correctifs appliqués

- `tests/e2e.rs:403` : `vc` → `_vc` (var inutilisée, clippy -D warnings).
- Commit et push à suivre (v0.7.5 ou inclus dans le prochain bump).

## 6. Recommandations (aucune urgente côté code)

1. **Réseau** : désactiver le VPN (ou faire une exception de routage pour
   `opencode.ai` hors tunnel) → les modèles free repassent immédiatement ;
   c'est le même VPN qui bloque SSH vers kali 192.168.40.15.
2. **Robustesse** (optionnel) : classifier la bannière « aren't available
   from your current network » comme un cas dédié (ex. `IpBanned`) avec
   message clair au client (« IP bannie par l'amont — vérifie ton VPN »)
   au lieu d'un `Transient` générique.
3. **Robustesse** (optionnel) : garder une trace IP de sortie (healthcheck
   ipify périodique) exposée dans `/admin/stats` pour diagnostiquer ce
   genre d'incident en un coup d'œil.
