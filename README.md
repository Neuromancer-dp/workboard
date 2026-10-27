# Chat

A private desktop chat for local [Ollama](https://ollama.com) models. Tauri v2 (Rust) + vanilla TypeScript.

- Everything is bundled: no CDN, no remote scripts, no telemetry, strict CSP. The webview has **no network access at all**.
- All network I/O (Ollama, DuckDuckGo) happens in Rust. Ollama is only ever contacted on `127.0.0.1`.
- Chats, attachments and settings live in one SQLite file: `~/.local/share/dev.bingus.chat/chat.db`.
- The only traffic that leaves the machine is the DuckDuckGo query, and only when the **web** toggle is on.

## Build

Needs: Rust, Node 20+, `webkit2gtk-4.1`, `libsoup-3.0` (Manjaro: `sudo pacman -S webkit2gtk-4.1 base-devel`).

```sh
npm install
npm run tauri build -- --bundles appimage     # same as `cargo tauri build`; the CLI comes from npm
```

The AppImage lands in `src-tauri/target/release/bundle/appimage/`.

If linuxdeploy fails with a `strip` error on a recent binutils, build with `NO_STRIP=true`.
If the AppImage can't mount (no FUSE), run it with `APPIMAGE_EXTRACT_AND_RUN=1`.

## Install (launcher entry + icon)

```sh
./packaging/install.sh                 # installs the newest AppImage from the build dir
./packaging/install.sh --uninstall     # removes the app; your chats are left alone
```

This puts the AppImage in `~/.local/share/chat-app/`, a `chat.desktop` in `~/.local/share/applications/`
and the icon in `~/.local/share/icons/hicolor/512x512/apps/`. Then search for "Chat" in your launcher.

## Hyprland / WebKitGTK notes

Tested on Hyprland (Wayland), Intel Iris Xe, system WebKitGTK 2.52: it renders correctly with **no** environment
variables. If you see a blank window, flicker or broken rendering on another GPU/driver, try, in this order:

| Symptom | Try |
| --- | --- |
| Blank/black window, Wayland protocol errors | `WEBKIT_DISABLE_DMABUF_RENDERER=1` |
| Glitches, tearing, flicker | `WEBKIT_DISABLE_COMPOSITING_MODE=1` |
| Still broken | `GDK_BACKEND=x11` (runs through XWayland) |

Put the variable in the launcher's `Exec=` line, e.g. `Exec=env WEBKIT_DISABLE_DMABUF_RENDERER=1 /home/you/.local/share/chat-app/Chat.AppImage`.

Hyprland window rule if you want it to float: `windowrule = float, class:^chat$`.

**Reduce effects** (Settings): turns off `backdrop-filter` blur and the glow animation. Blur is the expensive part in
WebKitGTK. On first launch the app measures its own frame time for ~1 s and switches this on by itself if rendering
looks slow (median frame > 24 ms, or ≤ 2 CPU cores); it tells you when it does. On the dev machine it stayed off.

## Behaviour worth knowing

- **Nothing is lost.** Your message is written to the database in the same transaction that starts the reply. The
  reply is accumulated in Rust and flushed about every 2 s, and written finally on finish, Stop, error, or app exit.
  After a crash, half-finished replies come back marked "interrupted" with the text that was saved.
- **Delete is soft.** Delete moves a chat to Trash (confirmation first) for 30 days; Trash has Restore, Delete
  forever (confirmation) and Empty. Only chats older than 30 days in Trash are removed automatically.
- **Web search** results are injected for that one turn and cited as [1] [2]; they are never stored in the context
  sent on later turns. Source links are stored with the reply and open in your system browser.
- **Ollama** is started if nothing answers on the configured port and stopped on quit *only if this app started it*.
  The port is configurable in Settings; the host is always `127.0.0.1` so chats can't be pointed at another machine.
- **Backup**: Settings → Backup database writes a consistent snapshot (SQLite `VACUUM INTO`, safe while the app runs).
  **Export all** writes JSON (including images as base64). **Export** in the header writes one chat as Markdown.
- Thinking models: `<think>…</think>` and Ollama's separate `thinking` field show as a collapsed, dimmed block and
  are not sent back to the model on later turns.
- Context usage is not trimmed for you: if history + PDFs exceed the chosen ctx, Ollama silently drops the oldest
  part. PDF chips warn when a PDF alone passes 80 % of ctx.
- PDFs are text-extracted (cut at 60 000 characters). Scanned PDFs have no text: the app says so; attach pages as images.
  Password-protected PDFs and PDFs relying on CJK CMaps are not supported.

## Tests

```sh
npm test                                    # frontend: markdown renderer, sanitizer, think-splitting, grouping, export
MD_FULL=1 npx vitest run -t "fresh render"  # slow exhaustive every-prefix streaming check
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

Rust tests cover the DDG parser (saved HTML fixtures in `src-tauri/tests/fixtures/`), the DB layer (create, append,
draft flush, crash recovery, soft delete, restore, purge, FTS search, migrations, export, backup) and the whole turn
pipeline against an in-process mock Ollama (streaming, mid-stream flush, cancel, mid-stream error, dropped connection,
unreachable Ollama, request shape).

## Dependencies

Rust (`src-tauri/Cargo.toml`):

| Crate | Why |
| --- | --- |
| `tauri`, `tauri-build` | the app shell |
| `tauri-plugin-single-instance` | a second launch focuses the running window |
| `tauri-plugin-window-state` | remembers window size/position |
| `tauri-plugin-dialog` | native "save as" for export/backup (the webview gets only `dialog:allow-save`) |
| `rusqlite` (`bundled`) | SQLite with FTS5, compiled in so the AppImage doesn't depend on the system's sqlite |
| `reqwest` (`rustls-no-provider`, `stream`, `json`) | Ollama + DuckDuckGo requests, streaming bodies |
| `rustls` (`ring`) | TLS provider for reqwest; chosen over the default aws-lc to avoid a heavier C build |
| `scraper` | parse DuckDuckGo's HTML (port of the prototype's parser) |
| `serde`, `serde_json` | IPC and stored JSON |
| `tokio` | `select!` for cancel/flush timer, `Notify` for cancellation |
| `futures-util` | `StreamExt` for the response byte stream |
| `base64` | image payloads in and out of the DB / Ollama |
| `libc` | SIGTERM to Ollama's process group on exit |

Links are opened by running `xdg-open` directly (scheme-checked: http, https, mailto), so there is no opener plugin.

JavaScript: `marked` (markdown), `dompurify` (sanitizer), `highlight.js` (core + 12 languages only), `pdfjs-dist`
(PDF text, lazy-loaded with its worker), `@tauri-apps/api` + `@tauri-apps/plugin-dialog`. Dev only: `vite`,
`typescript`, `vitest`, `jsdom`, `@tauri-apps/cli`.
