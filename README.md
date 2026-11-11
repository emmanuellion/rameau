<p align="center"><img src="assets/icon.png" width="128" alt="Rameau"></p>

# Rameau

Client git graphique rapide, écrit en Rust avec [GPUI](https://www.gpui.rs), pour macOS, Linux et Windows.
Historique en graphe coloré, commit ligne par ligne, rebase interactif, plusieurs projets, connexion GitHub.

## Installation

Télécharge la dernière version sur la page [Releases](https://github.com/emmanuellion/rameau/releases/latest).
**Git doit être installé** : Rameau s'appuie sur le `git` de ta machine pour les commits, push, merge…

- **macOS** (Apple Silicon et Intel) : ouvre le `.dmg`, glisse Rameau dans Applications.
  L'app n'est pas notarisée par Apple : au premier lancement, va dans
  **Réglages Système › Confidentialité et sécurité** et clique sur **Ouvrir quand même**
  (ou `xattr -dr com.apple.quarantine /Applications/Rameau.app`).
- **Windows 10/11** : installe [Git pour Windows](https://git-scm.com/download/win), dézippe l'archive,
  lance `rameau.exe` (SmartScreen : **Informations complémentaires › Exécuter quand même**).
- **Linux** (x64 et ARM) : `chmod +x rameau-*.AppImage` puis lance-le. Il faut un pilote Vulkan
  (`mesa-vulkan-drivers` sur Intel/AMD). Sans FUSE, utilise l'archive `.tar.gz`.

## Ce que fait Rameau

- **Projets** : page « Importer un projet » (ouvrir un dossier, cloner une adresse, créer un dépôt,
  ou glisser-déposer un dossier dans la fenêtre). Le sélecteur en haut à gauche passe d'un projet à l'autre.
- **Historique** : graphe coloré, une couleur stable par branche (`origin/x` a la même que `x`).
- **Commit** : cocher les fichiers, ou choisir des lignes/blocs dans le diff (Maj+clic pour une plage).
- **Diffs** : coloration syntaxique, défilement horizontal, par commit ou pour les modifications locales.
- **Branches** (clic droit) : checkout, merge, rebase, rebase interactif (réordonner, reformuler,
  fusionner, supprimer des commits), nouvelle branche, suppression locale ou sur le serveur.
- **Stash** : bouton dans la barre du haut ; clic droit sur un stash pour l'appliquer ou le supprimer.
- **Abandonner des modifications** : clic droit sur un fichier, ou sur des lignes du diff (avec confirmation).
- **Conflits** : bandeau « Merge/Rebase en cours » avec Continuer / Annuler l'opération.
- **Push / Pull / Fetch**, connexion GitHub, thème clair ou sombre, panneaux redimensionnables.

Raccourcis : ↑ ↓ parcourir l'historique, Échap fermer le diff, ⌘↩ (Ctrl+↩ sous Linux/Windows) valider le commit.

Les projets importés et les préférences sont dans `~/Library/Application Support/rameau/config.json`
(macOS), `~/.config/rameau/config.json` (Linux) ou `%APPDATA%\rameau\config.json` (Windows).

## Compiler depuis les sources

```sh
cargo run --release                      # rouvre le dernier projet, ou la page d'import
cargo run --release -- /chemin/du/projet # ouvre (et importe) ce projet
cargo test
```

Sous Linux (Ubuntu/Debian), dépendances de compilation :

```sh
sudo apt install build-essential pkg-config cmake clang git \
  libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libx11-xcb-dev libxcb1-dev libvulkan1 libzstd-dev
```

Empaqueter à la main : `scripts/package-macos.sh` (`.app` + `.dmg`) et `scripts/package-linux.sh`
(AppImage + `.tar.gz`).

## Publier une version

1. Change `version` dans `Cargo.toml`, puis `cargo check` (met à jour `Cargo.lock`), et commite.
2. `git tag v0.2.0 && git push origin main v0.2.0`

La pipeline [Release](.github/workflows/release.yml) compile macOS (universel), Linux (x64, ARM) et
Windows (x64), puis publie la release GitHub avec les paquets et leurs sommes SHA-256.
Elle refuse un tag qui ne correspond pas à la version de `Cargo.toml`.

## Connexion GitHub

Rameau utilise le *device flow* OAuth de GitHub : l'utilisateur clique sur « Connexion GitHub »,
le code est copié, la page GitHub s'ouvre, il colle le code et valide. Le token est rangé par le
gestionnaire d'identifiants de git (Trousseau sur macOS, Credential Manager sur Windows, libsecret s'il
est configuré) ; sans gestionnaire, dans `~/.config/rameau/credentials`, lisible par l'utilisateur seul.

Le *Client ID* de l'OAuth App Rameau est dans `.cargo/config.toml` (il est public : pas de secret
dans le binaire). Pour utiliser ta propre OAuth App : crée-la sur
https://github.com/settings/applications/new, coche **Enable Device Flow**, et remplace l'identifiant.

## Licence

Rameau est un logiciel libre sous licence [GPL-3.0-or-later](LICENSE).

Il s'appuie notamment sur [GPUI](https://github.com/zed-industries/zed) et
[gpui-component](https://github.com/longbridge/gpui-component) (Apache-2.0), [libgit2](https://libgit2.org)
(GPL-2.0 avec exception de liaison) et des icônes [Lucide](https://lucide.dev) (ISC, voir
`assets/icons/LICENSE`).
