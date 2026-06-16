import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Character, type CharacterState } from "./character";
import "./styles.css";

// --- Event payload contracts (must match the Rust `tauri_commands`/`actor`) ---
interface StatePayload {
  state: CharacterState;
}
interface TokenPayload {
  token: string;
}
interface DonePayload {
  full_text: string;
}

const stage = document.getElementById("stage") as HTMLElement;
const bubble = document.getElementById("bubble") as HTMLElement;
const stateLabel = document.getElementById("state-label") as HTMLElement;
const form = document.getElementById("ask-form") as HTMLFormElement;
const input = document.getElementById("ask-input") as HTMLInputElement;

const character = new Character(stage, stateLabel);

let streamBuffer = "";

async function bootstrap(): Promise<void> {
  // Character state transitions are driven entirely by the backend actor.
  await listen<StatePayload>("character://state", (e) => {
    character.set(e.payload.state);
  });

  // Token-by-token stream from the local LLM worker.
  await listen<TokenPayload>("llm://token", (e) => {
    streamBuffer += e.payload.token;
    bubble.textContent = streamBuffer;
  });

  await listen<DonePayload>("llm://done", (e) => {
    bubble.textContent = e.payload.full_text || streamBuffer;
    streamBuffer = "";
  });

  await listen<{ message: string }>("llm://error", (e) => {
    bubble.textContent = `⚠ ${e.payload.message}`;
    streamBuffer = "";
  });
}

form.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const prompt = input.value.trim();
  if (!prompt) return;
  input.value = "";
  streamBuffer = "";
  bubble.textContent = "";
  try {
    // Returns immediately: the command only forwards to the actor channel.
    await invoke("ask_assistant", { prompt });
  } catch (err) {
    bubble.textContent = `⚠ ${String(err)}`;
  }
});

bootstrap().catch((err) => console.error("bootstrap failed", err));
