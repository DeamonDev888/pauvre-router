# TODO — Router des Pauvres

## v0.3.x

- [ ] Auth admin (token) sur les endpoints `/admin/*` — actuellement localhost-only
- [ ] SSE / streaming (`stream: true`) — aujourd'hui réponse d'un bloc
- [ ] Persist du mode de routage dans le state (PAUVRE_MODE seulement au boot)
- [ ] Ré-import automatique de la liste `models:` d'Hermes depuis `/v1/models`

## v0.4+

- [ ] Poids pondérés dans `frontier-robin-free` (latence mesurée → répartition)
- [ ] Health-check passif périodique par modèle (au lieu d'apprendre sur échec)
- [ ] Métriques Prometheus (`/metrics`)
- [ ] Support multi-backends (au-delà d'OpenCode : autre CLI fingerprinté)
- [ ] Tests d'intégration HTTP bout-en-bout (spawn serveur éphémère)

## Idees

- [ ] Webhook Discord sur exhaustion cascade complète
- [ ] `--daemon` flag + tray Windows
