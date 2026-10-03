---
name: editeur-wow-sur-mesure
description: "UniWoW, éditeur « Unity pour WoW » 3.3.5 (serveur + client) dans E:\\WoW-editor — Rust + egui, une DLL par fonctionnalité ; jalon 1 VALIDÉ et POUSSÉ (03/10) ; copie de cette mémoire dans le dépôt"
metadata:
  node_type: memory
  type: project
  originSessionId: 987b6689-a1e4-4cb8-9006-72c07426f0e5
  modified: 2026-10-03T11:54:23.878Z
---

Projet ouvert le 2026-10-03 : **UniWoW**, un outil pour modifier intégralement serveur AzerothCore et client 3.3.5a, « une sorte d'Unity pour WoW ». Dépôt `E:\WoW-editor` (distant `Adrestie/UniWoW`, public), cahier des charges `docs/SPECIFICATION.md` (anglais) VALIDÉ : architecture + 25 fonctionnalités.

**État au 2026-10-03 : jalon 1 VALIDÉ et POUSSÉ sur `Adrestie/UniWoW` (d7fd0e2, écarts reportés dans la spec acceptés).** Contenu : runtime + noyau + `viewport` + deux échantillons (`sample-cube`, `sample-notes`) + `xtask`. Aucun fichier WoW encore. Prochaine étape non fixée (questions ouvertes en section 10 de la spec : modèle de projet, installations ciblées, ordre des fonctionnalités).

**Copie de cette mémoire dans le dépôt** : `docs/memory/editeur-wow-sur-mesure.md` (demande de l'utilisateur du 03/10). La mettre à jour à chaque push.

Décisions de l'utilisateur :
- **Joueurs avec WarcraftXL** : « la règle du "Client non modifié" ne tient pas pour l'éditeur ; la nouvelle règle : le joueur doit avoir WXL ». Portée énoncée pour l'éditeur ; l'extension aux mods existants (ForeverUI, BLP ≤ 1024 de [[wow-335-tampon-textures]]) n'est PAS tranchée.
- **Hors client**, **sur mesure** (Noggit Red [[noggit-red-build]] ne convient pas ; s'en inspirer seulement).
- **Rust + egui** (egui_dock, egui-wgpu) ; prix accepté : egui avant 1.0, version figée. Écartés : Slint, cxx-qt. Code en anglais ([[code-en-anglais]]).
- **Modularité stricte : une DLL par fonctionnalité, chargée au démarrage, sans recompiler l'éditeur** (« si je sors la DLL du rendu 3D du terrain, l'éditeur se lance mais sans la fenêtre 3D »). La vue 3D est la fonctionnalité `viewport`, pas le core.
- **Nom UniWoW** (crates `uniwow-`, `UniWoW.exe`). **Pousser à chaque validation** ([[commit-push-apres-modif]]).

Faits techniques établis en construisant le jalon 1 (tous dans la spec) :
- Runtime = `uniwow_api.dll` (crate `dylib` réexportant eframe/egui/wgpu/egui_dock/glam/log/serde) + `std-<hash>.dll` (`-C prefer-dynamic` dans `.cargo/config.toml`, sinon « std only shows up once »). Fonctionnalités = `cdylib`, entrée Rust `uniwow_feature_create` + C `uniwow_feature_package`. Le noyau est lié dans l'exe : le modifier ne rend pas les fonctionnalités incompatibles.
- **LNK1189** (65 535 symboles) si le profil dev n'est pas optimisé : `[profile.dev] opt-level = 2` → 17 764 symboles (27 %). `cargo xtask check` affiche le compte.
- Empreinte = BLAKE3 de `uniwow_api.dll` ; la compilation n'est pas déterministe : toute recompilation du runtime oblige à recompiler les fonctionnalités (`cargo xtask build`).
- **Piège egui_dock 0.21** : `retain_tabs` casse l'arbre quand il retire beaucoup d'onglets d'un coup (racine scindée aux enfants vides → assertion `old.is_leaf() || old.is_parent()` au démarrage suivant). Retirer onglet par onglet (`find_tab` + `remove_tab`), valider l'arbre, restauration protégée par `guarded`.
- Commandes : `cargo xtask new-feature|build|build-feature|run|check` ; sortie dans `out\debug` ; réglages `%APPDATA%\UniWoW\settings.json`, ou `UNIWOW_SETTINGS_DIR` pour les essais.
- Recette par Claude : scripts de capture et de clic dans le scratchpad de la session (PrintWindow + SetCursorPos) ; vérifier le focus avant tout clic (Windows le refuse quand l'utilisateur travaille ailleurs).

Environnement : Rust 1.99.0 (rustup, stable MSVC, rustfmt + clippy) dans `%USERPROFILE%\.cargo\bin` ; dans le Bash de l'outil, ajouter ce dossier au PATH.

Éléments vérifiés :
- wxl-core (github.com/WarcraftXL/wxl-core, C++, GPL-3.0) : DLL chargée par la table d'import de Wow.exe, accroches, appels typés, événements, formats ADT/WMO/M2/WDT/WDL en mémoire ; module d'exemple wxl-mini-noggit. Extensions : wxl-modern-m2/adt/wmo, wxl-db2.
- warcraft-rs (github.com/wowemulation-dev/warcraft-rs, Rust, MIT/Apache) : formats 1.12.1–5.4.8 dont 3.3.5a ; écriture confirmée MPQ, BLP, WMO, WDT ; à vérifier pour DBC, ADT, M2.

Mode Play visé : client connecté automatiquement (compte admin), changements poussés à chaud plutôt que redémarrage. Voir [[conception-pas-a-pas]].
