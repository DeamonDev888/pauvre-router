# Pauvre-Router 🗑️🚀

<p align="center">
  <img src="assets/banner.jpg" alt="Pauvre-Router 🗑️🚀" width="100%" />
</p>

<p align="center">
  <em>🇬🇧 English documentation: <a href="README_EN.md">README_EN.md</a></em>
</p>

<p align="center">
  <a href="#-démarrage-rapide"><img src="https://img.shields.io/badge/Rust-std--only-f97316?style=for-the-badge&logo=rust&logoColor=white" alt="Rust std-only" /></a>
  <a href="#-démarrage-rapide"><img src="https://img.shields.io/badge/Binaire-~650_KB-3b82f6?style=for-the-badge&logo=cargo&logoColor=white" alt="Binaire 650KB" /></a>
  <a href="#-les-modèles"><img src="https://img.shields.io/badge/Budget_API-0_€-10b981?style=for-the-badge&logo=free&logoColor=white" alt="0€" /></a>
  <a href="#-les-modèles"><img src="https://img.shields.io/badge/Modèles-5-ec4899?style=for-the-badge" alt="5 modèles" /></a>
  <a href="#-licence"><img src="https://img.shields.io/badge/License-MIT-green?style=for-the-badge" alt="MIT" /></a>
</p>

> **Un proxy local qui donne accès aux modèles frontière gratuits d'OpenCode Zen.**
> Tu veux coder avec des modèles de pointe sans payer ? Lance le binaire, branche Hermes, c'est tout.

---

## 🚀 Démarrage rapide

1. **Télécharge le binaire** : [Releases](../../releases) (Windows / Linux / macOS)
2. **Lance-le** — il écoute sur `127.0.0.1:4445`
3. **Branche ton client** — exemple Hermes (`config.yaml`) :

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

4. Redémarre Hermes → le groupe **PAUVRE-ROUTER** apparaît dans le picker :

<p align="center">
  <img src="assets/hermes-picker.png" alt="Groupe PAUVRE-ROUTER dans le picker Hermes" width="70%" />
</p>

💡 **`frontier-robin-free` = le mode "à l'aveugle"** : un seul modèle à choisir, il envoie chaque requête au meilleur modèle du moment tout seul.

Compilation depuis la source et guides multi-plateformes (auto-start, plugin Hermes) : **[SKILL.md](SKILL.md)**.

---

## 🤖 Les modèles

Tous gratuits, testés en live :

| Modèle | Vitesse | Pour quoi |
|---|---|---|
| 🔄 `frontier-robin-free` | *auto* | **Choix recommandé** — route tout seul vers le meilleur |
| `beam-free` | 5,3 s | 1M de contexte, le plus rapide |
| `glm-5.3-flash-free` | 6,4 s | Le plus malin pour le code |
| `space-bunny-free` | 5,7 s | Rapide pour le chat |
| `deepseek-v4.1-flash-free` | 9,0 s | Bon en analyse |

Si un modèle tombe (quota, panne), le suivant prend le relais automatiquement. Tu ne vois rien.

---

## ✨ Ce qu'il fait pour toi

- **Anti-coupure** — quota épuisé ou modèle down → bascule automatique sur le suivant
- **Robin intelligent** — apprend les latences réelles et envoie chaque requête au plus rapide fiable
- **Clés API en rotation** (optionnel) — ajoutes-en plusieurs, il gère les quotas tout seul
- **Anti-spam** — protections intégrées si tu lances des dizaines d'agents
- **Cache** — même question = réponse instantanée
- **Mise à jour** — il te prévient quand une nouvelle version sort (`/health` → `update.available`)
- **661 KB, zéro dépendance** — un seul fichier, ça tourne partout

---

## 📊 Surveillance

| URL | Quoi |
|---|---|
| `http://127.0.0.1:4445/health` | Vivant ? Quelle version ? Mise à jour dispo ? |
| `http://127.0.0.1:4445/admin/stats` | Stats complètes (latences, modèles, spam) |

---

## 📦 Releases & Tests

Binaires Windows / Linux / macOS construits par GitHub Actions : **[Releases](../../releases)**.

**86 tests** (unitaires + backend stub + E2E TCP), tous verts en dev et release — voir [CHANGELOG.md](CHANGELOG.md) pour l'historique.

---

## 📜 Licence

**MIT** — et le nom reste *Pauvre-Router*. 🗑️🚀
