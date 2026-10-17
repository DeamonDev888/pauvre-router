# Pitch Discord — copier/coller tel quel

⭐ **GLM 5.3 Flash is now FREE in OpenCode** ⭐

J'ai codé **le Router des Pauvres** 🗑️ — un proxy local qui donne accès aux **modèles frontière 100% gratuits** de Zen à n'importe quel client OpenAI-compatible (Hermes, scripts, curl…).

**Les frontières du router (bench live) :**
```
🥇 space-bunny-free           6,0 s
🥇 deepseek-v4.1-flash-free   6,0 s
⭐ glm-5.3-flash-free        11,3 s   ← frontière, maintenant gratuit
```
Plus **`frontier-robin-free`** : notre pseudo-modèle round-robin qui tourne tout seul sur les trois. 🎰

**Ce qu'il fait quand ça chauffe :**
- 🔄 **Cascade de fallback** : quota épuisé → bascule auto sur le modèle suivant
- 🔑 **Rotation de clés API toujours active** : multi-comptes en round-robin, clé quota-blocked = cooldown 30 min, fallback anonyme
- ⚖️ **2 modes** : `priority` (exhaust le meilleur avant de changer) ou `roundrobin` (répartit tout)
- 🎰 **`frontier-robin-free`** : la répartition de charge en un nom de modèle
- 🛡️ **Anti-spam** : token bucket, concurrence limitée, cache sha256 des doublons
- 📊 **Dashboard** : `/admin/stats` — santé par modèle, cooldowns, spam bloqué
- 🦀 **Rust std-only** : binaire 650 KB, zéro dépendance lourde

Zéro budget API. Que des modèles frontière gratuits. C'est le router des pauvres. 🚀

**Binaires prêts (GitHub Actions) :**
- 🪟 Windows : https://github.com/DeamonDev888/pauvre-router/releases/latest/download/pauvre-router-windows-amd64.zip
- 🐧 Linux : https://github.com/DeamonDev888/pauvre-router/releases/latest/download/pauvre-router-linux-amd64.tar.gz
- 🍎 macOS (Apple Silicon) : https://github.com/DeamonDev888/pauvre-router/releases/latest/download/pauvre-router-macos-arm64.tar.gz
- 🍎 macOS (Intel) : https://github.com/DeamonDev888/pauvre-router/releases/latest/download/pauvre-router-macos-amd64.tar.gz

Repo : https://github.com/DeamonDev888/pauvre-router
