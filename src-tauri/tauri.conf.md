# tauri.conf.json — Bartleby Tauri Configuration Reference

> **Note :** Le format JSON ne supporte pas les commentaires (`//` ou `/* */`).
> Ce fichier `.md` documente exhaustivement chaque champ de `tauri.conf.json`.
> Il doit être lu en parallèle du fichier JSON.

---

## Vue d'ensemble

`tauri.conf.json` est le fichier de configuration central de Tauri.
Il est lu **à la compilation** par `tauri_build::build()` (dans `build.rs`) et par
la CLI Tauri (`npm run dev` / `npm run build`).

Il contrôle :
- Les métadonnées de l'application (nom, version, identifiant)
- La fenêtre WebView (taille, titre, comportement)
- La sécurité (Content Security Policy)
- Le packaging (icônes, formats de bundle)
- L'intégration JavaScript (`withGlobalTauri`)

---

## Champ par champ

### `$schema`
```json
"$schema": "https://schema.tauri.app/config/2"
```
Indique à votre éditeur (VS Code, IntelliJ…) le schéma JSON à utiliser pour la
validation et l'autocomplétion. Ce n'est pas une valeur fonctionnelle pour Tauri —
elle est ignorée à l'exécution. Pointe vers le schéma JSON officiel de Tauri v2.
Permet de détecter des erreurs de configuration sans lancer la compilation.

---

### `productName`
```json
"productName": "Bartleby"
```
Le nom affiché de l'application. Utilisé dans :
- Le titre de la fenêtre OS (barre de titre native, taskbar, dock)
- Le nom du paquet `.deb` (`bartleby_0.1.0_amd64.deb`)
- L'entrée `.desktop` Linux (Applications menu)
- L'infos de l'exécutable Windows (.exe version metadata)
- Le bundle `.app` macOS

**À garder en sync avec :**
- `name` dans `Cargo.toml` (le nom du binaire compilé)
- La balise `<title>` dans `index.html`

---

### `version`
```json
"version": "0.1.0"
```
Version de l'application en [Semantic Versioning](https://semver.org/) : `MAJOR.MINOR.PATCH`.

- `MAJOR` : changement incompatible avec les versions précédentes
- `MINOR` : nouvelle fonctionnalité rétrocompatible
- `PATCH` : correction de bug rétrocompatible

Utilisée dans :
- Le nom du paquet bundle (`.deb`, `.dmg`, `.exe`)
- Les métadonnées du binaire compilé
- La fenêtre "À propos" (référencée depuis `crate::VERSION` dans `main.rs`)

**À garder en sync avec :**
- `version` dans `Cargo.toml`
- `pub const VERSION: &str = "0.1 Beta"` dans `src/main.rs`
- Le texte affiché dans `index.html` (modal À propos)

---

### `identifier`
```json
"identifier": "fr.bartleby.app"
```
Identifiant unique de l'application au format **reverse-DNS** (convention Java/Android/macOS).

Format : `tld.domaine.appname`
- `fr` : top-level domain (ou pays d'origine)
- `bartleby` : nom du projet/auteur
- `app` : suffixe par convention pour les apps

Cet identifiant est utilisé pour :
- **Linux** : nom du répertoire de configuration dans `~/.config/` (mais Bartleby
  utilise `dirs::config_dir().join("bartleby")` en Rust, indépendamment de ceci)
- **macOS** : `Bundle Identifier` (obligatoire pour notarization Apple)
- **Windows** : clé de registre pour les données de l'application (`HKCU\Software\…`)
- **AppImage / Flatpak** : identifiant de l'application dans le catalogue

**Doit être globalement unique.** Deux applications avec le même identifiant
peuvent entrer en conflit sur les systèmes de packaging et de mise à jour.

---

### `build`

Ce bloc configure la CLI Tauri (`@tauri-apps/cli`), qui orchestre le processus
de développement et de build.

#### `build.beforeDevCommand`
```json
"beforeDevCommand": ""
```
Commande shell exécutée **avant** `tauri dev`. Vide car Bartleby n'utilise pas
de bundler frontend (pas de Vite, Webpack, ou autre transpilateur).

Pour un projet Vite+React, ce serait `"npm run dev"` (démarre le serveur de dev
Vite en parallèle du processus Tauri).

#### `build.beforeBuildCommand`
```json
"beforeBuildCommand": ""
```
Commande shell exécutée **avant** `tauri build`. Vide pour la même raison.

Pour un projet Vite, ce serait `"npm run build"` (transpile le TypeScript/JSX,
bundle les modules ES, etc.).

#### `build.frontendDist`
```json
"frontendDist": "../src"
```
Chemin vers les fichiers frontend à embarquer dans le binaire.
Relatif à `src-tauri/` (le répertoire contenant ce `tauri.conf.json`).

`"../src"` pointe vers le répertoire `src/` à la racine du projet :
```
Bartleby_V01_Tauri/
├── src/               ← frontendDist pointe ici
│   ├── index.html
│   ├── main.js
│   ├── style.css
│   └── bartleby.svg
└── src-tauri/
    └── tauri.conf.json  ← on est ici, donc "../src" = src/
```

Tauri lit ce répertoire à la compilation et **embarque tous les fichiers** dans
le binaire Rust via des assets statiques. La WebView charge `index.html` depuis
ces assets embarqués, **sans serveur web**. L'application fonctionne hors ligne.

**Note pour un projet avec bundler :**
Pour Vite, ce serait `"../dist"` (le répertoire de sortie de `vite build`).

---

### `app`

Configuration de l'application Tauri elle-même.

#### `app.windows`

Tableau de configurations de fenêtres. Chaque entrée crée une fenêtre au démarrage.
Bartleby n'a qu'une seule fenêtre.

##### `app.windows[0].title`
```json
"title": "Bartleby"
```
Titre affiché dans la barre de titre native de l'OS.
Peut être overridé dynamiquement en Rust avec `window.set_title("…")`.

##### `app.windows[0].width` et `height`
```json
"width": 720,
"height": 680
```
Dimensions initiales de la fenêtre en **pixels logiques** (CSS pixels, pas pixels physiques).
Sur un écran HiDPI (Retina, 4K), la fenêtre sera rendue à 2× ou 3× en pixels physiques
mais occupera le même espace visuel.

`720 × 680` est choisi pour :
- Tenir confortablement sur les écrans 1080p (1920×1080) en laissant de la place pour
  d'autres fenêtres
- Être suffisamment large pour afficher les chemins de destination sans troncature
- Permettre au log panel d'afficher plusieurs lignes sans scroll immédiat

##### `app.windows[0].resizable`
```json
"resizable": true
```
`true` → l'utilisateur peut redimensionner la fenêtre en faisant glisser ses bords.
`false` → taille fixe (utile pour les fenêtres de dialogue ou les apps à taille fixe).
Bartleby bénéficie d'un redimensionnement pour agrandir le panneau de log.

##### `app.windows[0].center`
```json
"center": true
```
`true` → la fenêtre s'ouvre centrée dans l'écran principal au premier lancement.
`false` → la fenêtre s'ouvre à la position par défaut du gestionnaire de fenêtres (généralement
coin supérieur gauche, ou en cascade si plusieurs fenêtres sont ouvertes).
Centré est le comportement attendu pour une nouvelle application qui s'ouvre.

---

#### `app.security.csp`
```json
"csp": "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:"
```
**Content Security Policy** — une directive de sécurité navigateur qui contrôle
quels contenus la WebView peut charger et exécuter.

La CSP protège contre les attaques XSS (Cross-Site Scripting) : si du code malveillant
parvient à s'injecter dans le HTML (via des données utilisateur non-échappées, par exemple),
la CSP l'empêche de charger des ressources externes ou d'exécuter des scripts inline.

**Décomposition de la valeur :**

| Directive | Valeur | Signification |
|-----------|--------|---------------|
| `default-src` | `'self'` | Par défaut, toutes les ressources (scripts, styles, images, XHR…) ne peuvent venir que du même "origine" — ici, les assets embarqués dans le binaire |
| `style-src` | `'self' 'unsafe-inline'` | Les styles CSS peuvent venir des assets ET être inline (`<style>` tags, `style="…"` attributes). `'unsafe-inline'` est nécessaire ici car certains styles CSS sont générés dynamiquement par JS (e.g. `progressFill.style.width = '…'`) |
| `img-src` | `'self' data:` | Les images peuvent venir des assets ET des `data:` URIs (images encodées en base64 directement dans le HTML/CSS). Necessaire si des thumbnails ou icônes sont encodés en base64 |

**Ce que cette CSP BLOQUE :**
- Scripts JavaScript depuis des CDNs externes (`https://cdn.example.com/lib.js`)
- Requêtes AJAX/fetch vers des serveurs externes (important pour la confidentialité)
- Chargement d'iframes de sites tiers
- `eval()` et `new Function()` (bloqués par `default-src 'self'`)

**Implications pour les développeurs :**
Si vous souhaitez ajouter une librairie externe (e.g. un graphique), il faudra soit
l'embarquer dans `src/` (recommandé), soit assouplir la CSP (moins sécurisé).

---

#### `app.withGlobalTauri`
```json
"withGlobalTauri": true
```
**Champ critique — lire attentivement.**

`true` → Tauri expose son API JavaScript comme la variable globale `window.__TAURI__`.

**Pourquoi c'est nécessaire pour Bartleby :**
Sans `withGlobalTauri: true`, l'API Tauri n'est accessible que via des imports
ES modules :
```javascript
import { invoke } from '@tauri-apps/api/core';
```
Cela nécessite un bundler (Vite, Webpack) ou `type="module"` dans le `<script>`.

Bartleby n'utilise ni bundler ni modules ES. `main.js` est un script classique
(`<script src="main.js">` sans `type="module"`). Avec `withGlobalTauri: true`,
on peut appeler directement :
```javascript
window.__TAURI__.core.invoke("command_name", args)
window.__TAURI__.event.listen("event-name", handler)
window.__TAURI__.dialog.open({ directory: true })
```

**Considération de sécurité :**
En exposant `window.__TAURI__` globalement, n'importe quel code JS s'exécutant
dans la WebView peut appeler des commandes Tauri. La CSP ci-dessus protège contre
l'injection de scripts externes. Les permissions des commandes Tauri sont limitées
par les fichiers `capabilities/*.json`.

**Comportement en Tauri v1 :**
`window.__TAURI__` était TOUJOURS exposé en Tauri v1.
En Tauri v2, c'est opt-in via ce champ (meilleure sécurité par défaut).

---

### `bundle`

Configuration du packaging de l'application en installateurs distribu­ables.

#### `bundle.active`
```json
"active": true
```
`true` → `tauri build` génère des fichiers de bundle (installateurs).
`false` → `tauri build` compile seulement le binaire Rust sans créer d'installateur.

Laisser `true` pour produire le `.deb` Linux, `.AppImage`, `.dmg` macOS, etc.

#### `bundle.icon`
```json
"icon": ["icons/bartleby.png"]
```
Liste des fichiers d'icône à embarquer dans le bundle.
Chemin relatif à `src-tauri/`.

Tauri redimensionne automatiquement les icônes PNG pour tous les formats requis :
- **Linux** : formats PNG multiples (16, 32, 64, 128, 256 px) pour les menus, la barre des tâches
- **macOS** : génère un `.icns` avec plusieurs résolutions (nécessite macOS pour la compilation)
- **Windows** : génère un `.ico` multi-résolution (peut être fait depuis Linux avec `tauri build`)

**Recommandation :** Fournir une image PNG 1024×1024 px pour une qualité optimale
à toutes les résolutions. Tauri se charge du redimensionnement.

**Chemin recommandé :** `src-tauri/icons/bartleby.png`

**Astuce :** La commande `tauri icon path/to/source.png` génère automatiquement
toutes les variantes de taille et de format nécessaires.

#### `bundle.targets`
```json
"targets": "all"
```
Quels formats de bundle générer lors de `tauri build`.

`"all"` → génère tous les formats disponibles pour la plateforme courante :
- **Linux** : `.deb` (Debian/Ubuntu/Mint), `.AppImage` (universel), `.rpm` (si configuré)
- **macOS** : `.dmg`, `.app`
- **Windows** : `.exe` (NSIS installer), `.msi` (WiX installer)

Pour ne générer qu'un seul format :
```json
"targets": ["deb"]
```
ou
```json
"targets": ["deb", "appimage"]
```

**Pour créer le .deb Bartleby :**
```bash
cd ~/Bureau/Bartleby_V01_Tauri
npm run build
# Le .deb est dans : src-tauri/target/release/bundle/deb/bartleby_0.1.0_amd64.deb
sudo dpkg -i src-tauri/target/release/bundle/deb/bartleby_0.1.0_amd64.deb
```

---

## Fichiers complémentaires (non inclus ici)

### `capabilities/default.json`
Déclare les permissions Tauri v2 accordées à la fenêtre principale.
Bartleby nécessite :
```json
{
  "identifier": "default",
  "description": "Default capabilities for Bartleby",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "dialog:default",
    "dialog:allow-open",
    "dialog:allow-save",
    "dialog:allow-message",
    "dialog:allow-ask",
    "dialog:allow-confirm"
  ]
}
```
Sans `dialog:allow-open`, `window.__TAURI__.dialog.open()` sera bloqué par le système
de permissions Tauri v2 et la sélection de dossier échouera silencieusement.

### `Cargo.toml` (src-tauri/Cargo.toml)
Décrit les dépendances Rust. Voir `Cargo.toml` commenté.

### `build.rs` (src-tauri/build.rs)
Script de build Rust. Voir `build.rs` commenté.
