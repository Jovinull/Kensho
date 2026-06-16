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
export KENSHO_MODEL_PATH=/path/to/qwen.gguf
export KENSHO_CTX=2048               # optional: context window (default 2048)
cd src-tauri
cargo build --features llama         # compiles llama.cpp (needs cmake + C++)
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

## IPC surface

Commands: `ask_assistant`, `create_task`, `list_tasks`, `send_notification`,
`set_always_on_top`, `app_info`.
Events emitted: `character://state`, `llm://token`, `llm://done`, `llm://error`.
