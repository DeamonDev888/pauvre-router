# Pitch Discord — copier/coller tel quel

⭐ **GLM 5.3 Flash is now FREE in OpenCode** ⭐

J'ai codé **le Router des Pauvres** 🗑️ — un proxy local qui donne accès aux modèles frontière **100% gratuits** de Zen à n'importe quel client OpenAI-compatible (Hermes, scripts, curl…).

**Le bench de latence (testé en live) :**
```
🥇 space-bunny-free            6,0 s
🥇 deepseek-v4.1-flash-free    6,0 s
🥉 longcat-2.5-preview-free    8,4 s
4  mimo-v2.6-flash-free        9,8 s
⭐ glm-5.3-flash-free         11,3 s   ← frontière, maintenant gratuit
🐌 nemotron-3.5-lightning     29,2 s
```

**Ce qu'il fait quand ça chauffe :**
- 🔄 **Cascade de fallback** : quota épuisé → bascule auto sur le modèle suivant
- 🔑 **Rotation de clés API** : multi-comptes en round-robin, clé quota-blocked = cooldown 30 min, fallback anonyme
- ⚖️ **2 modes** : `priority` (exhaust le meilleur avant de changer) ou `roundrobin` (répartit tout)
- 🛡️ **Anti-spam** intégré : token bucket, plafond de concurrence, cache sha256 des doublons
- 📊 **Dashboard** : `/admin/stats` — santé par modèle, cooldowns, spam bloqué
- 🦀 **Rust std-only** : binaire 650 KB, zéro dépendance lourde, démarrage instantané

Zéro budget API. Que des modèles frontière gratuits. C'est le router des pauvres. 🚀

GitHub : https://github.com/DeamonDev888/pauvre-router
