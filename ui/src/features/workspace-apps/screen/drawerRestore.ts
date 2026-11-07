import type { ScreenDrawerSize } from "./screenProtocol";

/** CAD-2221 — the drawer surface a connect return asked the board to re-open. */
export interface DrawerRestore { surface: string; size?: ScreenDrawerSize }

/**
 * The one-shot restore of a connect return's drawer (see `socialConnect`). The hint is taken
 * only once a primary frame is READY and can actually open the surface: a first mount that
 * fails keeps the hint for the retry that succeeds, and that ready mount opens it exactly
 * once. Another frame identity (install, digest, tag, context) starts its own latch.
 */
export class DrawerRestorer {
  private latched: string | null = null;
  constructor(private readonly scope: string) {}
  take(state: "loading" | "ready" | "fallback", hint: DrawerRestore | null): DrawerRestore | null {
    if (state !== "ready" || hint === null) return null;
    const key = `${this.scope}:${hint.surface}:${hint.size ?? ""}`;
    if (this.latched === key) return null;
    this.latched = key;
    return hint;
  }
}
