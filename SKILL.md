---
name: pauvre-router-install
description: "Use when installing the pauvre-router (Router des Pauvres) binary and its Hermes desktop monitoring plugin — download URLs, startup config per OS, plugin placement, verification steps."
version: 1.0.0
platforms: [windows, linux, macos]
---

# Installer le Router des Pauvres + plugin Hermes

Guide d'installation complet : binaire → auto-start → plugin desktop → vérification.

**Règle absolue : jamais de clé API dans un fichier versionné.** Les clés se
passent au router via `POST /admin/keys/add` (HTTP) — jamais dans un yaml/json
committé. Les fichiers d'état (`~/.pauvre-router-keys.json`,
`~/.pauvre-router-state.json`) vivent hors du repo et sont déjà dans `.gitignore`.

## 1. Le binaire

Télécharger la release pour ton OS (page releases du repo) :

| OS | Fichier |
|---|---|
| Windows | `pauvre-router-windows-amd64.zip` |
| Linux | `pauvre-router-linux-amd64.tar.gz` |
| macOS (Apple Silicon) | `pauvre-router-macos-arm64.tar.gz` |
| macOS (Intel) | `pauvre-router-macos-amd64.tar.gz` |

Extraire, puis rendre exécutable (unix) : `chmod +x pauvre-router`.

Le router délègue au CLI officiel OpenCode. Sur Windows il est auto-détecté ;
sur **Linux/macOS, pointer la variable d'env** `PAUVRE_OPENCODE` vers le
binaire `opencode` avant de lancer :

```bash
PAUVRE_OPENCODE=/usr/local/bin/opencode ./pauvre-router
```

## 2. Auto-start au login

**Windows** — copier `deploy/Zen_OC_Bridge.vbs` dans le dossier Startup en
ajustant le chemin de l'exe :

```
%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\
```

**Linux (systemd user)** — `~/.config/systemd/user/pauvre-router.service` :

```ini
[Unit]
Description=Router des Pauvres
[Service]
ExecStart=%h/bin/pauvre-router
Restart=on-failure
[Install]
WantedBy=default.target
```

Puis `systemctl --user enable --now pauvre-router`.

**macOS (launchd)** — `~/Library/LaunchAgents/dev.pauvre.router.plist` avec un
`ProgramArguments` pointant vers le binaire, puis
`launchctl load ~/Library/LaunchAgents/dev.pauvre.router.plist`.

## 3. Le plugin desktop Hermes

Copier le dossier `plugin/` du repo dans le dossier desktop-plugins de ton
profil Hermes (chemins relatifs à `$HERMES_HOME`, la racine Hermes) :

- profil par défaut : `$HERMES_HOME/desktop-plugins/pauvre-router/`
- profil nommé : `$HERMES_HOME/profiles/<profil>/desktop-plugins/pauvre-router/`

Le dossier de destination doit contenir exactement :

```
pauvre-router/
├── manifest.json
└── plugin.js
```

Puis dans l'app desktop : ⌘K → **Reload desktop plugins**. Le pane
« Router des Pauvres » apparaît (santé des modèles, cooldowns, stats, mode
de routage en badge). Un plugin désactivé dans Settings → Plugins le reste
au restart — le réactiver à la main.

## 4. Brancher un client (Hermes, etc.)

Dans le `config.yaml` du profil — la liste `models:` est **statique**, c'est
elle que lit le picker de modèles :

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

Redémarrer le client après modification.

## 5. Vérification (dans l'ordre)

```bash
curl -s http://127.0.0.1:4445/health          # {"status":"ok","router":"pauvre-router"}
curl -s http://127.0.0.1:4445/v1/models       # frontier-robin-free + 4 frontières
curl -s -X POST http://127.0.0.1:4445/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"frontier-robin-free","messages":[{"role":"user","content":"PING ?"}]}'
curl -s http://127.0.0.1:4445/admin/stats     # stats, santé, pool de clés, mode
```

Ajouter une clé au pool de rotation (jamais dans un fichier committé) :

```bash
curl -s -X POST http://127.0.0.1:4445/admin/keys/add -d '{"key":"sk-..."}'
```

## Pièges connus

- **Anonyme vs clé** : chez Zen, une clé dans l'env du spawn désactive le
  mode anonyme. Le router gère ça seul (jamais de mix) — ne pas contourner.
- **Port déjà pris** : `bind 127.0.0.1:4445 KO` → un autre process écoute
  (`netstat -ano | grep 4445`), le tuer puis relancer.
- **`opencode` non trouvé sur unix** : `PAUVRE_OPENCODE` obligatoire.
- **Plugin invisible** : vérifier le chemin (`desktop-plugins/` du BON
  profil), le nom de dossier = l'`id` du manifest, puis Reload desktop plugins.
