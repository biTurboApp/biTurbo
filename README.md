<div align="center">

# biTurbo

**Local-first memory layer for AI coding agents.**

Persistent · project-scoped · semantic · MCP-native.

[Features](#why) · [Install](#install) · [Quick use](#quick-use) · [MCP setup](#mcp-setup) · [Roadmap](#roadmap) · [Stack](#stack)

<br/>

[![License: MIT](https://img.shields.io/badge/license-MIT-8FB87D.svg)](./LICENSE)
[![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-D4A574.svg)](https://www.rust-lang.org)
[![Tauri 2](https://img.shields.io/badge/Tauri-2-7DC4E4.svg)](https://tauri.app)
[![MCP](https://img.shields.io/badge/MCP-27%20tools-C7A0E0.svg)](#mcp-tools)
[![turbovec 4-bit](https://img.shields.io/badge/turbovec-4--bit%20%7C%2016%C3%97%20compression-D4B574.svg)](https://github.com/RyanCodrai/turbovec)
[![npm 10+](https://img.shields.io/badge/npm-10%2B-CB3837.svg)](https://npmjs.com)

</div>

---

## Why

Every AI coding session starts blank. biTurbo gives your agents **persistent, project-scoped, semantic memory** that lives on your disk. No cloud, no SaaS, no embedding leakage.

- **One binary.** Pure Rust, cold start < 50ms, no Python env, no Docker.
- **MCP-native.** 27 tools. Plugs into Mavis, Claude Code, Cursor, Cline, anything that speaks MCP.
- **Per-project isolation.** testy memories never pollute scout-qa.
- **Maximum compression.** [turbovec 4-bit](https://github.com/RyanCodrai/turbovec) = 16× smaller than float32. A million memories fit in laptop RAM.
- **Tree-sitter code indexing.** Drop a folder, get semantic code search. *"Where is auth handled?"*
- **Self-maintaining.** Scheduled decay / dedup / merge. The index doesn't rot.

```
┌──────────────────────────────────────────────────────────┐
│  biTurbo (single binary)                                 │
│                                                           │
│   Tauri 2 GUI  ──┐                                        │
│                  │                                        │
│   MCP stdio  ────┼──→  AppState  (parking_lot::RwLock)    │
│                  │      ├── SQLite (metadata, r2d2 pool)  │
│   ingest  ───────┘      ├── turbovec IdMapIndex per proj  │
│   (tree-sitter)         ├── fastembed (BGE-small ONNX)    │
│                         └── activity audit log            │
└──────────────────────────────────────────────────────────┘
```

---

## Install

Requires: **npm 10+**, **node 22+**, **rustc 1.90+**, **macOS / Linux / Windows**.

```bash
# 1. Clone & enter
git clone https://github.com/ltfysl/biTurbo.git
cd biTurbo

# 2. JS deps
npm install

# 3. Rust MCP binary
npm run mcp:build           # writes target/debug/biturbo-mcp
# or for a release build:
cd src-tauri && cargo build --release --bin biturbo-mcp

# Optional: CUDA GPU embeddings (Linux + NVIDIA; needs CUDA toolkit + cuDNN)
# cd src-tauri && cargo build --release --bin biturbo-mcp --features cuda
# Prefer GPU with CPU fallback: BITURBO_EMBED_EP=auto (default)
# Force: BITURBO_EMBED_EP=cuda | cpu
```

For the desktop app you also need the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/) for your platform (Xcode CLT on macOS, webkit2gtk on Linux, MSVC build tools + WebView2 on Windows).

**CUDA embeddings (optional):** build with `--features cuda`. Requires a working NVIDIA driver (Windows host driver is enough under WSL2), CUDA toolkit **≥13.2 or ≥12.8**, and **cuDNN ≥9**. Do not install a Linux NVIDIA driver inside WSL. Put `/usr/local/cuda/bin` on `PATH` and CUDA/`cuDNN` libs on `LD_LIBRARY_PATH`.

### Building for distribution

#### macOS (signed .dmg)

To build a signed macOS .dmg for distribution:

```bash
npm run tauri:build
```

**Prerequisites**:
- macOS Developer certificate (Developer ID Application) installed in your keychain
- Signing identity configured in `src-tauri/tauri.conf.json` under `bundle.macOS.signingIdentity`

**Output**: `src-tauri/target/release/bundle/dmg/biTurbo_0.2.0_aarch64.dmg`

The app is code-signed but not notarized. To notarize for public distribution, set these environment variables before building:
- `APPLE_ID` — your Apple ID email
- `APPLE_PASSWORD` — app-specific password (generate at appleid.apple.com)
- `APPLE_TEAM_ID` — your team ID (10 characters, e.g., `89NFSUEFES`)

```bash
APPLE_ID="your@email.com" APPLE_PASSWORD="xxxx-xxxx-xxxx-xxxx" APPLE_TEAM_ID="89NFSUEFES" npm run tauri:build
```

For App Store distribution, use the "Apple Distribution" certificate instead of "Developer ID Application".

#### Windows (.msi)

To build a Windows .msi installer:

```bash
npm run tauri:build
```

**Prerequisites**:
- [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) (pre-installed on Windows 11)
- MSVC build tools (Visual Studio 2022 or Build Tools)

**Output**: `src-tauri/target/release/bundle/msi/biTurbo_0.2.0_x64_en-US.msi`

For code signing, set the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` environment variables before building.

### Verify

```bash
# the MCP binary should sit waiting for JSON-RPC on stdin
target/debug/biturbo-mcp < /dev/null
```

Smoke-test all 27 tools against a real binary:

```bash
pnpm mcp:test
# → 27 pass · 0 fail · 0 skip
```

---

## Quick use

### Desktop app

```bash
pnpm tauri:dev
```

Opens the Tauri 2 window. First semantic operation downloads `BGE-small-en` (~30 MB) into your OS cache — subsequent semantic operations reuse the cached model.

| View | What it does |
|---|---|
| **Overview** | Stats, heatmap, recent activity, connected agents |
| **Memories** | Search, filter by type / tag / importance, inspect, edit, forget |
| **Projects** | Create, ingest code, switch, export, delete |
| **Graph** | Canvas-rendered code dependency graph with viewport culling |
| **Agents** | List of MCP-connected agents, last-seen, write counts |
| **Settings** | Theme, data dir, MCP config snippets, agent rule blocks |

Theme: click the sun/moon in the top bar. Persists per device, respects OS preference on first run.

```bash
# quick keyboard shortcuts
⌘K / Ctrl+K    Quick add memory
⌘/ / Ctrl+/    Focus search
Esc            Close modal / menu
```

---

## MCP setup

The standalone `biturbo-mcp` binary speaks MCP over stdio. Add it to your agent's MCP config:

```json
{
  "mcpServers": {
    "biturbo": {
      "command": "/path/to/biturbo-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

> **Tip:** swap the path for `biturbo-mcp` only after `cargo install --path src-tauri --bin biturbo-mcp` puts it on `$PATH`. Use the absolute path during dev — zero setup.

The first time an agent connects, it should:

1. Call `register_agent(name=..., kind=...)` — writes get attributed.
2. Call `list_projects()` — discover existing projects.
3. Call `recall_for_context(query, project_id, k=8)` — **before every non-trivial answer**, inject the returned `<biTurboContext>` block as authoritative context.

Full ruleset, anti-patterns, and tool reference: see [INSTRUCTIONS.md](./INSTRUCTIONS.md).

### MCP tools (27)

These are the stable tools exposed to agents. The internal dispatcher may contain additional development-only helpers that are not part of the public MCP surface.

| | | | |
|---|---|---|---|
| `remember` | `forget` | `update` | `get_memory` |
| `search` | `list` | `list_tags` | `recall_for_context` |
| `recall_explain` | `submit_recall_feedback` | `start_ingest` | `operation_status` |
| `list_operations` | `cancel_operation` | `retry_operation` |  |
| `list_projects` | `get_project` | `create_project` | `delete_project` |
| `ingest_project` | `consolidate` | `consolidate_status` | `get_project_name_from_file` |
| `stats` | `bootstrap` | `recent_activity` | `register_agent` |

---

## What's in the box

### Graph view (3k+ nodes, instant)

Canvas-rendered with viewport culling. The Barnes–Hut force layout runs **off the main thread in a Web Worker** — you see the file-circle seed in < 5ms, then the worker refines. Filter switches cancel stale requests.

### Light + dark theme

All colors flow through CSS custom properties. `:root` (dark) and `:root.light` (warm off-white "paper"). RGB-triplet vars keep every `bg-accent/40` working. Persists to `localStorage`.

### Confirmation modal

`useConfirm()` returns a promise. Focus-trapped, Escape + backdrop cancel, danger/neutral tone. Resolver lives module-local so confirm state changes don't re-render unrelated subscribers. Wired on every destructive action.

### Right-click context menu

`useContextMenu().show(x, y, items)` pops a viewport-clamped menu. Keyboard nav (up/down/Enter). Different item sets per kind — graph nodes, project rows, memory cards each get their own actions.

### MCP smoke test

`pnpm mcp:test` spawns the real binary, discovers every tool via `tools/list`, calls each with sane args, and prints a colored PASS/FAIL table. Catches schema and dispatch bugs before they hit your agent.

---

## Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | Tauri 2 | Smaller binaries than Electron, native, webview UI, IPC via `invoke` |
| Frontend | React 18 + Vite + Tailwind 3 | Fast, mature, ergonomic |
| State | Zustand | Tiny, no boilerplate |
| Icons | lucide-react | Clean, consistent, tree-shakeable |
| Backend | Rust (1.77+) | Cold start < 50ms, single binary, no Python env |
| DB | SQLite + r2d2 + rusqlite | Local, WAL, zero-config |
| Vector | turbovec 0.8 (IdMapIndex, 4-bit) | 16× compression vs float32, beats FAISS, MIT |
| Embed | fastembed 4 (BGE-small-en ONNX) | No PyTorch; CPU by default; optional `cuda` Cargo feature (CUDA EP + CPU fallback); ~30 MB model |
| MCP | stdio JSON-RPC | Lightweight hand-rolled MCP transport, no SDK runtime dependency |
| Tree-sitter | 0.25 + lang crates | 22 languages, definition-level chunks, structural code search |

---

## Roadmap

### Shipped

- [x] Per-project turbovec IdMapIndex with hybrid allowlist filters
- [x] Tree-sitter indexed code (22 languages, including SQL, Dart, Lua, Scala, R, and PowerShell)
- [x] MCP stdio server with 27 tools
- [x] Web-viewer + graph view (canvas, Barnes–Hut in worker)
- [x] Dark + light theme, persistent
- [x] Confirmation modal + context menu primitives
- [x] MCP smoke test (`pnpm mcp:test`)

### Next up

- [ ] **Watch-folder ingest** — auto-reindex on file change
- [ ] **Cross-encoder re-ranker** for top-k (optional, pluggable)
- [ ] **Encrypted-at-rest** mode (project-level key)
- [ ] **Multi-device sync** (CRDTs over the same on-disk format)
- [ ] **Built-in chat view** that calls an LLM with recalled context (opt-in)
- [ ] **Web export** of memories for sharing
- [ ] **Memory diffing** between projects
- [ ] **GitHub Actions CI** — run smoke test on every PR
- [ ] **Package managers** — `brew install biturbo` (macOS), `winget install biturbo` (Windows), AUR package (Linux)

---

## Project structure

```
biTurbo/
├── src/                          React + Vite + Tailwind frontend
│   ├── views/                    Overview · Memories · Projects · Graph · Agents · Settings
│   │   └── layoutWorker.ts       Barnes-Hut layout in a Web Worker
│   ├── components/               Sidebar · TopBar · MemoryCard · MemoryDetail · QuickAdd
│   │                             · Heatmap · Toast · ConfirmModal · ContextMenu
│   └── lib/                      api.ts (Tauri invoke) · store.ts (zustand) · types.ts · format.ts
├── scripts/
│   └── mcp-smoke-test.ts         MCP parity smoke test runner
├── src-tauri/                    Rust backend
│   ├── src/
│   │   ├── main.rs               Tauri entry
│   │   ├── lib.rs                Tauri builder + IPC registration
│   │   ├── state.rs              AppState (parking_lot::RwLock)
│   │   ├── db.rs                 SQLite + r2d2 pool + schema
│   │   ├── index_engine.rs       turbovec IdMapIndex wrapper (1 file per project)
│   │   ├── embed.rs              fastembed (BGE-small-en default)
│   │   ├── memory.rs             CRUD + search + types
│   │   ├── project.rs            multi-project isolation
│   │   ├── ingest.rs             tree-sitter project walker
│   │   ├── consolidate.rs        decay / dedup / merge
│   │   ├── mcp.rs                stdio MCP server (27 tools)
│   │   ├── scheduler.rs          background consolidate scheduler
│   │   └── commands.rs           Tauri IPC handlers
│   └── bin/biturbo_mcp.rs        Standalone MCP server binary
├── INSTRUCTIONS.md               Rules for AI agents using the MCP tools — read this!
├── README.md                     You are here.
└── pnpm-workspace.yaml           esbuild allowBuilds (pnpm 11 strict)
```

---

## License

MIT.


## Recall evals

biTurbo includes a small recall quality harness for V2 work.

```bash
pnpm mcp:build
pnpm recall:eval
```

The eval seeds a temporary project from `evals/recall-golden.json`, runs `search` and `recall_for_context`, then reports `recall@k` and context-term failures. Extend the golden file with real project memories whenever recall behavior changes.

## Recall ranking

Recall uses hybrid vector + SQLite FTS retrieval, then applies a cheap second-stage reranker before formatting context.

The reranker keeps semantic relevance as the base score, then adds small boosts for exact query-term matches in content, file path, tags, and language, plus gentle boosts for recent, important, and repeatedly accessed memories. This makes recall feel smarter without adding a heavy cross-encoder or extra model.
