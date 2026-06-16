/**
 * Character state machine. Mirrors the backend `CharacterState` emitted on the
 * `character://state` event, plus a frontend-only `alert` state for errors.
 * Swaps the rendered SVG sprite per state; the motion lives in CSS (60fps).
 */
export type CharacterState = "idle" | "thinking" | "speaking" | "alert";

export type SpriteMap = Record<CharacterState, string>;

export class Character {
  private state: CharacterState = "idle";

  constructor(
    private readonly stage: HTMLElement,
    private readonly sprite: HTMLImageElement,
    private readonly label: HTMLElement,
    private readonly sprites: SpriteMap,
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
    // [data-state] drives the CSS animation; the <img> shows the matching SVG.
    this.stage.dataset.state = this.state;
    this.sprite.src = this.sprites[this.state];
    this.label.textContent = this.state;
  }
}
