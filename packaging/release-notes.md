## Installation

| Système | Fichier |
|---|---|
| macOS (Apple Silicon et Intel) | `rameau-…-macos-universal.dmg` |
| Windows 10/11 (x64) | `rameau-…-windows-x86_64.zip` |
| Linux (x64 / ARM) | `rameau-…-linux-x86_64.AppImage` ou `…-aarch64.AppImage` (ou les `.tar.gz`) |

**Git doit être installé** : Rameau s'appuie sur le `git` de ta machine pour les commits, push, merge…

### macOS
1. Ouvre le `.dmg` et glisse **Rameau** dans **Applications**.
2. L'app n'est pas signée par Apple : au premier lancement, macOS refuse de l'ouvrir.
   Va dans **Réglages Système › Confidentialité et sécurité**, puis clique sur **Ouvrir quand même** en face de Rameau.
   (Ou dans un terminal : `xattr -dr com.apple.quarantine /Applications/Rameau.app`.)

### Windows
1. Installe [Git pour Windows](https://git-scm.com/download/win) si ce n'est pas déjà fait.
2. Dézippe l'archive et lance `rameau.exe`. Windows SmartScreen peut afficher « Windows a protégé votre ordinateur » :
   clique sur **Informations complémentaires › Exécuter quand même**.

### Linux
```sh
chmod +x rameau-*-linux-x86_64.AppImage
./rameau-*-linux-x86_64.AppImage
```
Il faut un pilote Vulkan (`mesa-vulkan-drivers` sur Intel/AMD, le pilote propriétaire sur NVIDIA).
Si l'AppImage ne démarre pas (FUSE absent), utilise l'archive `.tar.gz`.

L'intégrité des fichiers peut être vérifiée avec `SHA256SUMS.txt`.
