# DSH Studio

A native desktop window for the [`dsh`](https://www.npmjs.com/package/@deepseek-ai/dsh) web coding agent.

DSH Studio runs the `dsh web` backend locally on a random loopback port, waits
until it serves, and shows it in a native macOS window. The agent UI itself is
dsh's own — this project is only the shell around it.

## Features

- **Self-contained** — bundles its own `node` + `dsh`, no install required
- **dsh profile picker** — switch between `~/.dsh/profiles/*` from the
  Backend menu (plugins and custom bundles load in the desktop app too)
- **External links open in your browser** — the agent window stays clean
- **Crash diagnostics** — when the backend dies, the splash screen shows the
  tail of its output; the supervisor restarts it with backoff
- **Close-to-tray** — closing the window keeps the backend running
- Window state memory, zoom, devtools, menu bar

## Build

```sh
npm install
./scripts/stage-backend.sh   # stage node + dsh into resources/backend
npx tauri build --bundles app
```

Dev mode: `DSH_BIN=$(which dsh) npm run dev` (uses dsh from PATH).

## License

MIT for the shell code here. The bundled engine, `@deepseek-ai/dsh`, is MIT
and is staged locally at build time — it is not redistributed in this repo.
