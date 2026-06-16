# Kensho

Autonomous local-AI desktop companion for Ubuntu. A transparent, always-on-top
floating character driven by a **local Qwen `.gguf`** model, with persistent
memory, agenda and tasks.

Stack: **Tauri v2** · **Rust** (Tokio actor) · **Vanilla TypeScript + Vite**
(chosen over a framework to keep RAM minimal).

## Architecture (Clean / DDD)

```
src/                       Frontend (Vanilla TS, low-RAM)
  main.ts                  IPC + event-stream wiring
  character.ts             Idle / Thinking / Speaking state machine
  styles.css               Transparent window + 60fps CSS animation

src-tauri/src/
  core/                    Errors, config, logging (cross-cutting)
  domain/                  Pure entities: Task, ScheduleEvent, MindMapNode,
                           UserProfile + strongly-typed IDs
  infrastructure/
    database/              SQLite (rusqlite, bundled) + auto-migration
    llm/                   InferenceEngine trait, MockEngine, gguf backend
    os_signals/            Native notifications via D-Bus (notify-rust)
  services/                Business orchestration (AssistantService)
  actor/                   Persistent Tokio LLM worker (the concurrency core)
  tauri_commands/          IPC transport exposed to the frontend
```

### Concurrency model (why the UI never freezes)

A single long-lived **Tokio actor** owns the inference engine. Tauri commands
never call the model directly — they push a message onto an `mpsc` channel via
`LlmHandle`. The actor streams tokens back as Tauri events
(`llm://token` → `llm://done`) and drives the character state
(`character://state`: Idle → Thinking → Speaking → Idle). The main/UI thread is
never blocked, so the floating character keeps animating at 60fps. Disk I/O
(SQLite) runs on `spawn_blocking`.

## Prerequisites (Ubuntu)

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```
Plus a recent Rust toolchain and Node.js.

## Run

```bash
npm install
npm run tauri dev        # dev (hot-reload frontend + Rust)
npm run tauri build      # production bundle (.deb / AppImage)
```

Backend-only checks:
```bash
cd src-tauri
cargo check
cargo build
```

## Local model (real inference)

The default build ships a **MockEngine** that fakes token streaming, so the full
pipeline runs with no model and no C++ build. To use a real Qwen `.gguf`:

```bash
./scripts/download_qwen.sh            # downloads a tiny Qwen2.5-0.5B GGUF into .models/
export KENSHO_MODEL_PATH=$(pwd)/.models/qwen2.5-0.5b-instruct-q4_k_m.gguf
export KENSHO_CTX=2048               # optional: context window (default 2048)
npm run tauri dev -- --features llama # run the app with the real engine
```

Build prerequisites for the `llama` feature (bindgen needs clang headers):
```bash
sudo apt install clang libclang-dev   # provides clang resource headers (stdbool.h…)
# If only libclang runtime is present, point bindgen at gcc's headers instead:
export LIBCLANG_PATH=/usr/lib/llvm-18/lib
export BINDGEN_EXTRA_CLANG_ARGS="-I/usr/lib/gcc/x86_64-linux-gnu/13/include -I/usr/include"
```

The system depends only on the `InferenceEngine` trait, so swapping the mock for
the real `llama-cpp-2` backend touches nothing outside `infrastructure/llm/`.
The single integration TODO (the decode loop) is marked in
`src-tauri/src/infrastructure/llm/llama.rs`.

## Interacting with Kensho

- **Global hotkey** `Ctrl+Shift+K` (or double-click the character) → focuses the
  window and slides in a translucent Spotlight-style input. `Enter` sends the
  prompt and hides the input; `Esc` dismisses it.
- **Tool calling**: Kensho can act on the system. The model emits an inline tag,
  which the actor's stream filter strips from the visible text, routes through
  the `ToolRouter`, executes, and confirms with a native notification + a
  transient on-screen **toast** (`tool://executed`). Built-in tools:

  ```
  <CALL:ADD_TASK>Comprar pão|2026-06-20</CALL>        # personal task (date optional)
  <CALL:DELEGATE>Rafaela|Corrigir bug no login</CALL>  # ticket → team member
  <CALL:READ_FILE>/var/log/app/error.log</CALL>        # read + analyze a file
  <CALL:CMD>git status</CALL>                          # run a shell command
  <CALL:SCAN_DIR>/home/me/paper-draft</CALL>           # summarize a whole dir
  ```

  Robustness: a **fuzzy parser** (`actor/stream_filter.rs`) also accepts drifted
  syntax small models emit — `[CALL: ADD_TASK]…[/CALL]`, `<call:add_task>…`,
  case/space variants. Optionally, set `KENSHO_GRAMMAR=1` to enable a **lazy GBNF
  grammar** (`grammar_lazy`) that hard-constrains tag structure once `<CALL:`
  appears, leaving free text unconstrained.

  - `DELEGATE` validates the assignee against the dev team
    (`Waldston`, `Joãozinho`, `Rafaela`), stores an agile-issue payload in the
    `delegated_tasks` table, and (if `KENSHO_TEAM_WEBHOOK_URL` is set) POSTs a
    JSON notification to a Slack/Discord-style endpoint. Network failure is
    non-fatal — the ticket persists and the summary notes the failure.
  - `READ_FILE` reads a clamped slice (first/last 100 lines, ≤1 MB) and injects
    it back into the rolling window, triggering a follow-up generation.
  - `CMD` runs `sh -c <cmd>` with a strict 5s timeout (`kill_on_drop`), truncates
    output to the last 2000 chars, and injects it back for analysis. **Mutating
    commands** (anything outside a safe-prefix allowlist, or containing shell
    metacharacters) trigger **human-in-the-loop approval**: the backend emits
    `ui://require-approval`, suspends the tool on a oneshot channel, and waits for
    `approve_action` (`[Y/N]` in the UI; 60s → auto-deny).
  - `SCAN_DIR` walks a directory (depth/file-capped), summarizes each text file
    rule-based (Markdown headers, Rust/Python signatures, TeX sections), and
    injects a bounded digest — RAG-lite for docs that exceed the context window.

## Proactivity (heartbeat)

A `tokio::time::interval` in the actor ticks every `KENSHO_HEARTBEAT_SECS`
(default 300). On each tick it queries SQLite for tasks due today that are still
pending; for any not yet nudged this session it fires a native notification,
switches the sprite to `alert`, and has Kensho generate a reminder.

  Tags split across streamed tokens are handled by `actor/stream_filter.rs`.
  Adding a capability = implement the `Tool` trait + `register()` it — nothing
  else changes (MCP-ready).

## IPC surface

Commands: `ask_assistant`, `create_task`, `list_tasks`, `send_notification`,
`set_always_on_top`, `app_info`.
Events emitted: `character://state`, `llm://token`, `llm://done`, `llm://error`,
`tool://executed`; consumed: `ui://focus-input` (from the global hotkey).
