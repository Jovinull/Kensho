import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Character, type CharacterState, type SpriteMap } from "./character";

// SVG assets resolved to URLs by Vite.
import idleUrl from "./assets/character/idle.svg";
import thinkingUrl from "./assets/character/thinking.svg";
import speakingUrl from "./assets/character/speaking.svg";
import alertUrl from "./assets/character/alert.svg";

import "./styles.css";

// --- Event payload contracts (must match the Rust `actor` module) ---
// Backend now also emits "alert" (proactive heartbeat reminders).
interface StatePayload {
  state: CharacterState;
}
interface TokenPayload {
  token: string;
}
interface DonePayload {
  full_text: string;
}
interface ErrorPayload {
  message: string;
}
interface ToolPayload {
  summary: string;
}
interface ApprovalPayload {
  id: string;
  command: string;
}
interface StartSentencePayload {
  text: string;
  duration_ms: number;
}

const stage = document.getElementById("stage") as HTMLElement;
const sprite = document.getElementById("sprite") as HTMLImageElement;
const bubble = document.getElementById("bubble") as HTMLElement;
const stateLabel = document.getElementById("state-label") as HTMLElement;
const characterEl = document.getElementById("character") as HTMLElement;
const toast = document.getElementById("toast") as HTMLElement;
const form = document.getElementById("ask-form") as HTMLFormElement;
const input = document.getElementById("ask-input") as HTMLInputElement;

const sprites: SpriteMap = {
  idle: idleUrl,
  thinking: thinkingUrl,
  speaking: speakingUrl,
  alert: alertUrl,
};

const character = new Character(stage, sprite, stateLabel, sprites);

let streamBuffer = "";
let alertTimer: number | undefined;
let toastTimer: number | undefined;

// Transient action confirmation under the character (3s).
function showToast(text: string): void {
  toast.textContent = text;
  toast.classList.add("show");
  if (toastTimer !== undefined) clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toast.classList.remove("show");
    toastTimer = undefined;
  }, 3000);
}

// --- Spotlight-style input (hidden by default; slides in on focus) ---
function openInput(): void {
  stage.dataset.inputOpen = "true";
  // Defer focus until the slide transition begins.
  window.setTimeout(() => input.focus(), 20);
}

function closeInput(): void {
  stage.dataset.inputOpen = "false";
  input.blur();
}

// Human-in-the-loop: show the command and resolve on Y/N.
let approvalActive = false;
function requestApproval(req: ApprovalPayload): void {
  if (approvalActive) return; // one at a time
  approvalActive = true;
  character.set("alert");
  if (toastTimer !== undefined) clearTimeout(toastTimer);
  toast.textContent = `Permitir?  ${req.command}   [Y / N]`;
  toast.classList.add("show", "approve");

  const finish = async (approved: boolean) => {
    document.removeEventListener("keydown", onKey, true);
    approvalActive = false;
    toast.classList.remove("show", "approve");
    try {
      await invoke("approve_action", { id: req.id, approved });
    } catch (err) {
      console.error("approve_action failed", err);
    }
  };

  const onKey = (ev: KeyboardEvent) => {
    const k = ev.key.toLowerCase();
    if (k === "y" || k === "enter") {
      ev.preventDefault();
      void finish(true);
    } else if (k === "n" || k === "escape") {
      ev.preventDefault();
      void finish(false);
    }
  };
  document.addEventListener("keydown", onKey, true);
}

function showAlert(): void {
  character.set("alert");
  if (alertTimer !== undefined) clearTimeout(alertTimer);
  alertTimer = window.setTimeout(() => {
    alertTimer = undefined;
    character.set("idle");
  }, 2500);
}

async function bootstrap(): Promise<void> {
  // Global hotkey (Ctrl+Shift+K) routed from the Rust backend.
  await listen("ui://focus-input", () => openInput());

  // Dangerous shell command awaiting human approval.
  await listen<ApprovalPayload>("ui://require-approval", (e) => {
    requestApproval(e.payload);
  });

  // Backend actor is the source of truth for idle/thinking/speaking.
  await listen<StatePayload>("character://state", (e) => {
    if (character.current === "alert" && alertTimer !== undefined) return;
    character.set(e.payload.state);
  });

  await listen<TokenPayload>("llm://token", (e) => {
    streamBuffer += e.payload.token;
    bubble.textContent = streamBuffer;
  });

  await listen<DonePayload>("llm://done", (e) => {
    bubble.textContent = e.payload.full_text || streamBuffer;
    streamBuffer = "";
  });

  await listen<ErrorPayload>("llm://error", (e) => {
    bubble.textContent = `⚠ ${e.payload.message}`;
    streamBuffer = "";
    showAlert();
  });

  // A tool ran on the backend (e.g. task added / delegated / file read) —
  // surface a discreet, transient toast confirming the background action.
  await listen<ToolPayload>("tool://executed", (e) => {
    showToast(`✓ ${e.payload.summary}`);
  });

  // Fake lipsync: pulse the sprite while a sentence is actually being voiced.
  let talkTimer: number | undefined;
  await listen<StartSentencePayload>("audio://start-sentence", (e) => {
    sprite.classList.add("talking");
    if (talkTimer !== undefined) clearTimeout(talkTimer);
    // Safety auto-clear in case the end event is missed.
    talkTimer = window.setTimeout(
      () => sprite.classList.remove("talking"),
      e.payload.duration_ms + 800,
    );
  });
  await listen("audio://end-sentence", () => {
    if (talkTimer !== undefined) clearTimeout(talkTimer);
    sprite.classList.remove("talking");
  });
}

// Double-click the character to reveal the input too.
characterEl.addEventListener("dblclick", () => openInput());

// Esc closes the input.
input.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") closeInput();
});

form.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const prompt = input.value.trim();
  if (!prompt) return;
  input.value = "";
  streamBuffer = "";
  bubble.textContent = "";
  closeInput(); // hide the input; the character takes over (thinking…)
  try {
    // Returns immediately: the command only forwards to the actor channel.
    await invoke("ask_assistant", { prompt });
  } catch (err) {
    bubble.textContent = `⚠ ${String(err)}`;
    showAlert();
  }
});

bootstrap().catch((err) => console.error("bootstrap failed", err));
