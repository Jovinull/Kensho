/**
 * Character state machine. Mirrors the backend `CharacterState` emitted on
 * the `character://state` event. Kept tiny and dependency-free for low RAM.
 */
export type CharacterState = "idle" | "thinking" | "speaking";

export class Character {
  private state: CharacterState = "idle";

  constructor(
    private readonly stage: HTMLElement,
    private readonly label: HTMLElement,
  ) {
    this.apply();
  }

  set(state: CharacterState): void {
    if (state === this.state) return;
    this.state = state;
    this.apply();
  }

  get current(): CharacterState {
    return this.state;
  }

  private apply(): void {
    // Drives CSS via [data-state]; the 60fps animation lives purely in CSS.
    this.stage.dataset.state = this.state;
    this.label.textContent = this.state;
  }
}
