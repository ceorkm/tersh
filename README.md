<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="frontend/public/brand/tersh-mark-dark.png" />
  <img src="frontend/public/brand/tersh-mark.png" alt="Tersh" width="150" height="150" />
</picture>

# Tersh

### The SSH client your AI agent has been waiting for.

*Drag a screenshot onto your terminal. It's in your agent's prompt before you let go of the mouse.*

![License](https://img.shields.io/badge/license-AGPL--3.0-2ea44f?style=flat-square)
![Platform](https://img.shields.io/badge/macOS%20·%20Windows%20·%20Linux-1f2937?style=flat-square)
![Built with](https://img.shields.io/badge/Tauri%202%20·%20Rust-orange?style=flat-square)
![Frontend](https://img.shields.io/badge/React%2019%20·%20xterm.js-2563eb?style=flat-square)
![Telemetry](https://img.shields.io/badge/telemetry-none-2ea44f?style=flat-square)

</div>

---

Tersh is an open-source **SSH / SFTP client built for the era of remote AI agents**: Claude Code, Codex, aider, Gemini CLI. It does everything a great terminal does, and one thing nothing else does: it makes the wall between *your machine* and *the agent running on your server* disappear.

It's **local-only**. No account. No cloud sync. No telemetry. No phoning home. Your hosts, keys, and secrets live in an encrypted vault on your disk and nowhere else.

---

## ✦ Why Tersh exists

I was scrolling Twitter one day and someone asked, basically: *"why can't my SSH client just let me upload a picture straight to the agent I'm talking to?"*

And I stopped, because I do this **constantly**. Screenshot an error, a design, a diagram… and then comes the ritual: find the file, upload it to the server by hand, switch apps, paste the path in, get it wrong, try again. Every single time.

Nobody had built the simple thing, so I did. Then I kept going. Tersh became the craziest, most fun thing I've made in years, and it's open source.

---

## ✦ The wedge

**Today's flow with a remote AI agent:**

```
take screenshot → find the file → upload it to the server → switch apps → paste the path → fix the path
```

**The Tersh flow:**

```
drag → done
```

When you drag any file onto a terminal where Claude Code / aider / Codex / Gemini CLI is running, Tersh:

1. **SFTP-uploads** it to `~/.tersh/uploads/<host>/<session>/<filename>` over the SSH connection you already have open. No new auth, no second tool.
2. **Detects which agent** is in the foreground.
3. **Types the remote path into the prompt**, formatted for that agent: `@path` for Gemini, `/add path` for aider, the raw path for Claude Code / Codex.
4. **Never presses Enter for you.** The path lands in the buffer. *You* decide when to send.

There's also an explicit **"Upload to agent"** button in every tab, and agent-aware paste detection so the reference is always formatted right.

---

## ✦ The Prompt Enhancer: sharpen your prompt, for cents

Tersh ships a built-in **Prompt Enhancer** that turns a half-formed thought into a tight, repo-grounded prompt for your coding agent.

Hit **Ctrl+P** on a rough prompt. Tersh hands it to an AI model of your choice. You bring your own key, and the cheap ones are more than enough (**DeepSeek**, or any model on **OpenRouter**, often the free ones). That model reads your *actual* code on the server (opening files, searching, mapping the project) and rewrites your rough ask into a sharp one that names your real files and functions. Then you paste *that* into Claude or Codex.

- 🧠 **It reads the repo, not the vibes.** A per-connection **Project Index** ("brain") lets the model ground the prompt in your real code.
- 🔁 **It rewrites, never answers.** A question stays a sharper question; "build X" stays a tighter build task. The output is a *prompt for your agent*, never a substitute for it.
- 🔒 **Bring your own key.** It lives in the encrypted vault, never leaves your machine. Use OpenRouter, DeepSeek, or a custom endpoint.
- 🗂️ **Per-VPS isolation.** Open 50 servers and each has its own project + context. The index is stored **inside the project folder on the server** (`<root>/.tersh/`), never on your Mac.

---

## ✦ Collaborator Mode: build with every agent in view

Flip into **Collaborator Mode** and the terminal becomes a **shared workbench**: open as many terminals and agents as you want, arranged **side-by-side on one canvas**, with a **shared file-tree explorer** down the side. Run Claude on one box, Codex on another, a local shell in the third. **Compare, steer, and ship without tab-hunting.**

- 🪟 **Many terminals, one canvas.** Add as many panes as you need; each is a real, live session.
- 🌳 **Shared file tree.** Browse the project across every pane from a single explorer.
- 🔀 **Parallel agent work.** Drive several AI agents at once and keep them all in view.

---

## ✦ Everything else it does

<table>
<tr><td valign="top" width="50%">

**🔌 Connection**
- Password + public-key (`ed25519` / `rsa` / `ecdsa`)
- **ProxyJump / jump-host chains** (multi-hop)
- Per-host env vars (SSH `SetEnv`) + startup snippet
- Configurable keepalive · **auto-reconnect** with backoff
- **Host groups & tags** for organizing saved hosts
- **Local terminals too**, not just SSH

**🛰️ Port forwarding**
- **Local** (`-L`): listen locally, dial through SSH
- **Remote** (`-R`): server listens, forwards to you
- **Dynamic** (`-D`): full SOCKS5, no-auth, CONNECT
- Saved, managed, start/stop per tunnel

**📁 SFTP**
- File browser: navigate, sort, search
- Upload (drag-drop, picker, "upload here") · Download (native dialog)
- Rename · mkdir · chmod · move · delete · copy-path
- **In-app file preview**
- Sensitive-path warnings (`.pem`, `.kdbx`, `.env`, `.ssh/`, `id_*`, `wallet.*`, cloud creds)

</td><td valign="top" width="50%">

**🔑 Keys & Vault**
- **SSH key management**: generate, import, list, delete
- Encrypted SQLite vault at rest (**AES-256-GCM**, key in OS keychain)
- Encrypted export/import (**Argon2id** + AES-256-GCM, your passphrase)
- Optional: SSH key passphrase in OS keychain

**🧰 Workflow**
- **Snippets**: save commands, run them on a session, grouped
- **Session logs**
- **Known Hosts** manager: every pinned fingerprint, first-seen, copyable
- **OS auto-detection** + per-host OS badges
- **Agent detection**: Claude Code · Codex · aider · Gemini · cursor-agent

**🛡️ Security & ⌨️ UX**
- Host-key **TOFU** · **OSC 52 blocked** · drag-drop **never auto-sends**
- Sensitive-path defense-in-depth (frontend **and** backend)
- **Cmd+K** command palette (fuzzy hosts, snippets, settings)
- **14 themes · 7 fonts** · adjustable text size
- Tab strip · per-host **HostInspector** slide-in · hash routing

</td></tr>
</table>

---

## ✦ Architecture

```
tersh/
├── frontend/                 ← React 19 · xterm.js 5.5 · Vite 6 · TypeScript
│   └── src/{App.tsx, components/, lib/}
└── backend/                  ← Rust · Tauri 2 · russh 0.45 · rusqlite
    └── src/
        ├── ssh/              ← russh client + host-key pinning
        ├── sftp/             ← upload/download/rename/chmod/sensitive-path
        ├── vault/            ← encrypted SQLite + AES-GCM + keychain
        ├── tunnels/          ← local / remote / dynamic forwarders
        ├── brain/            ← per-VPS project index for the Prompt Enhancer
        └── agent_detect/     ← detect the remote AI agent process
```

**Rust to the metal.** Tauri 2 keeps the shell native and the binary small. SSH is pure-Rust `russh`, no libssh2 C surface. SQLite is bundled via `rusqlite`, so there's no system-lib dependency to drift.

---

## ✦ Building

```sh
# Install JS deps
npm ci --ignore-scripts

# Dev (hot-reload Vite + tauri dev)
npm run tauri:dev

# Production build
npm run tauri:build
```

Bundles land in `backend/target/release/bundle/`.

---

## ✦ Releases

CI (`.github/workflows/release.yml`) builds and **signs**:

- macOS universal `.app` + `.dmg`: Developer ID + notarized
- Windows x64 + arm64 `.msi`: Authenticode-signed
- Linux AppImage + `.deb` + `.rpm` + `tar.gz`

Tag `v*.*.*` and the workflow draft-publishes the GitHub Release with all artifacts attached. (Signing secrets are listed in the workflow.)

---

## ✦ License

**AGPL-3.0.** See [`LICENSE`](./LICENSE). Run it, fork it, hack it. If you host a modified version as a network service, you owe your changes back. That's the whole deal.

---

## ✦ Contributing

**Not taking pull requests right now.** 🙅 Tersh is a single-maintainer project *on purpose*. Every line that ships gets read by one person who cares too much about it.

**But the issue tracker is wide open.** 🐛 Found a bug, a rough edge, or a feature you'd kill for? **Open an issue.** That's exactly the signal I want. For security issues, please report them privately first rather than in a public issue.

<div align="center">

---

*Built with obsession. Made because a tweet annoyed me into it.*

</div>
