# DSH Studio

A native desktop app for the [`dsh`](https://www.npmjs.com/package/@deepseek-ai/dsh) coding agent.

DSH Studio runs the `dsh web` backend locally on a random loopback port, waits
until it serves, and opens it in a native window. The agent UI is dsh's own —
this project is only the shell: process supervision, native chrome, and a few
comforts the browser tab doesn't give you.

```
┌─────────────────────────────────────┐
│  DSH Studio (Tauri shell)           │  window, menu bar, tray,
│  supervisor + profile picker        │  restart with backoff
├─────────────────────────────────────┤
│  dsh web engine (npm, MIT)          │  runs on 127.0.0.1:<random>
└─────────────────────────────────────┘
```

## Features

- **Self-contained** — the app bundles its own `node` + `dsh`; nothing to install
- **dsh profile picker** — switch between `~/.dsh/profiles/*` from the
  **Backend** menu. Custom bundles and plugins load in the desktop app too,
  so a pluginized dsh setup works here exactly as in the terminal
- **Supervised backend** — crashes are restarted with exponential backoff
  (1s → 30s); the splash screen shows the tail of the backend output so you
  can see *why* it died
- **External links open in your default browser** — the agent window stays on
  your conversation
- **Close-to-tray** — closing the window hides the app; the backend keeps running
- **Open in Browser…** (⌘⇧B) — jump to the same backend in any browser
- Window-state memory, zoom (⌘ +/-/0), devtools, standard macOS menus

## Download

Grab the latest `.dmg` / `.app` from
[Releases](https://github.com/madearga/dsh-studio/releases)
(builds are currently macOS arm64; see below to build for other platforms).

> **Note:** the app is not signed with an Apple Developer certificate. On first
> launch macOS may block it — right-click → Open, or clear the quarantine flag:
> ```sh
> xattr -c "/Applications/DSH Studio.app"
> ```

## Build from source

Prerequisites: [Rust](https://rustup.rs), Node.js 20+, Xcode Command Line Tools.

```sh
npm install
./scripts/stage-backend.sh   # stage node + dsh into resources/backend
npx tauri build --bundles app
```

The staged engine comes from your local npx cache if one exists, otherwise
from npm (`@deepseek-ai/dsh`, pinned in `resources/backend`).

**Dev mode** uses `dsh` from your `PATH` instead of the bundle:

```sh
DSH_BIN=$(command -v dsh) npm run dev
```

### Custom engine version

`scripts/stage-backend.sh` picks whatever `@deepseek-ai/dsh` it finds. To pin
a specific version:

```sh
rm -rf resources/backend
DSH_SOURCE=/path/to/node_modules ./scripts/stage-backend.sh
```

## How it works

1. On launch, a supervisor thread picks a free loopback port and spawns
   `dsh web --host 127.0.0.1 --port <port>` (bundled binary, `DSH_BIN` if set,
   or `dsh` from `PATH`).
2. The splash page shows boot status; when the port accepts connections, the
   window navigates to it.
3. If the backend exits, the supervisor captures the last output lines, shows
   them on the splash screen, and restarts after a growing delay.
4. Quitting (⌘Q / tray) kills the child; closing the window just hides to tray.

## Project layout

```
src/                 splash page (plain HTML/CSS/JS, no bundler)
src-tauri/src/       Rust shell: supervisor, menus, tray, navigation guard
scripts/             stage-backend.sh (engine staging), helpers
assets/icon.png      icon source (regenerate with: npx tauri icon assets/icon.png)
```

## License

MIT — see [LICENSE](LICENSE). Applies to this shell only.
The engine, [`@deepseek-ai/dsh`](https://github.com/deepseek-ai/deepseek-harness),
is MIT software by DeepSeek and is fetched at build time; it is not
redistributed in this repository.
