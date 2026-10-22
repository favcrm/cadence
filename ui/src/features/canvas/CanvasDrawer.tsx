import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { composerDraft, setComposerDraft } from "../app-shell/chat/composerStore";
import ArtifactView, { downloadName, useArtifactBytes } from "./ArtifactView";
import {
  canvasMode,
  clampWidth,
  closeDrawer,
  closeTab,
  focusTab,
  readWidth,
  selectVersion,
  useCanvas,
  writeWidth,
  type CanvasTab,
  type Working,
} from "./canvasStore";

/** Versions the store keeps per artifact (the daemon prunes older ones). */
const KEPT_VERSIONS = 20;

function useViewport(): number {
  const [w, setW] = useState(() => window.innerWidth);
  useEffect(() => {
    const on = () => setW(window.innerWidth);
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, []);
  return w;
}

/** The running turn in plain words: the agent's latest commentary and the live step. */
function WorkingStrip({ working }: { working: Working }) {
  const t = useTranslate();
  return (
    <div className="canvas-working" role="status" aria-live="polite" data-canvas-working>
      <span className="canvas-working-label num">{t("Working")}</span>
      {working.plan ? <p className="canvas-working-plan">{working.plan}</p> : null}
      {working.step ? <p className="canvas-working-step num">{working.step}</p> : null}
    </div>
  );
}

/** The passage the operator selected inside the document, or "". */
function selectedPassage(root: HTMLElement | null): string {
  const sel = window.getSelection();
  if (!root || !sel || sel.isCollapsed || !sel.anchorNode || !root.contains(sel.anchorNode)) return "";
  return sel.toString().trim().slice(0, 1200);
}

function Body({ conversation, tab, composerKey, working }: {
  conversation: string;
  tab: CanvasTab;
  composerKey: string | null;
  working: Working | null;
}) {
  const t = useTranslate();
  const state = useArtifactBytes(conversation, tab.id, tab.selected);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const first = Math.max(1, tab.latest - KEPT_VERSIONS + 1);
  const versions: number[] = [];
  for (let v = first; v <= tab.latest; v++) versions.push(v);

  const download = () => {
    if (state.status !== "ready") return;
    const url = URL.createObjectURL(new Blob([state.data.bytes], { type: state.data.mime.split(";")[0] }));
    const a = document.createElement("a");
    a.href = url;
    a.download = downloadName(tab.title, tab.kind, state.data.mime);
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  // Ask to change: the selected passage (or, with none, the artifact itself)
  // goes into the composer as a quoted reference. Nothing is sent.
  const askToChange = () => {
    if (!composerKey) return;
    const passage = selectedPassage(contentRef.current);
    const head = `Re: "${tab.title}" v${tab.selected}`;
    const quote = passage ? `\n${passage.split("\n").map((l) => `> ${l}`).join("\n")}` : "";
    const existing = composerDraft(composerKey);
    setComposerDraft(composerKey, `${existing ? `${existing}\n\n` : ""}${head}${quote}\n\nChange: `);
    document.getElementById("app-shell-chat-box")?.focus();
  };

  return (
    <>
      {working ? <WorkingStrip working={working} /> : null}
      <div className="canvas-body" data-canvas-body>
        {state.status === "loading" ? <p className="text-label text-ink-500">{t("Loading…")}</p> : null}
        {state.status === "failed" ? (
          <p className="text-label text-fail" role="alert">{state.error}</p>
        ) : null}
        {state.status === "ready" ? (
          <ArtifactView kind={tab.kind} title={tab.title} data={state.data} contentRef={contentRef} />
        ) : null}
      </div>
      {!working ? (
        <div className="canvas-foot" data-canvas-foot>
          <label className="canvas-versions text-micro text-ink-400">
            <span className="num">{t("Version")}</span>
            <select
              value={tab.selected}
              onChange={(e) => selectVersion(tab.id, Number(e.currentTarget.value))}
              aria-label={t("Version")}
            >
              {versions.map((v) => (
                <option key={v} value={v}>v{v}{v === tab.latest ? ` · ${t("latest")}` : ""}</option>
              ))}
            </select>
          </label>
          <span className="flex-1" />
          <button type="button" className="btn" onClick={download} disabled={state.status !== "ready"}>
            {t("Download")}
          </button>
          <button type="button" className="btn" onClick={askToChange} disabled={!composerKey}>
            {t("Ask to change")}
          </button>
        </div>
      ) : null}
    </>
  );
}

/**
 * The agent canvas (CAD-1726): one non-modal drawer docked to the right,
 * mounted once at the shell. push / cover / sheet by the room the page has;
 * resizable by pointer and arrow keys; Esc closes; no scrim, no focus trap.
 */
export default function CanvasDrawer() {
  const t = useTranslate();
  const canvas = useCanvas();
  const viewport = useViewport();
  const [width, setWidth] = useState(readWidth);
  const w = clampWidth(width, viewport);
  const mode = canvasMode(viewport, w);
  const open = canvas.open && canvas.tabs.length > 0 && canvas.conversation !== null;
  const tab = canvas.tabs.find((x) => x.id === canvas.active) ?? canvas.tabs[0] ?? null;

  // The page makes room (push) or steps aside through these two attributes.
  useLayoutEffect(() => {
    const root = document.documentElement;
    if (!open) {
      root.removeAttribute("data-canvas");
      root.style.removeProperty("--canvas-w");
      return;
    }
    root.setAttribute("data-canvas", mode);
    root.style.setProperty("--canvas-w", `${w}px`);
    return () => {
      root.removeAttribute("data-canvas");
      root.style.removeProperty("--canvas-w");
    };
  }, [open, mode, w]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) closeDrawer();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  const commit = useCallback((px: number) => {
    const next = clampWidth(px, window.innerWidth);
    setWidth(next);
    writeWidth(next);
  }, []);

  const drag = useRef<{ x: number; w: number } | null>(null);
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    drag.current = { x: e.clientX, w };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    setWidth(clampWidth(drag.current.w + (drag.current.x - e.clientX), window.innerWidth));
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current = null;
    e.currentTarget.releasePointerCapture(e.pointerId);
    writeWidth(w);
  };
  const onResizeKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 96 : 24;
    if (e.key === "ArrowLeft") commit(w + step);
    else if (e.key === "ArrowRight") commit(w - step);
    else return;
    e.preventDefault();
  };

  if (!open || !tab || canvas.conversation === null) return null;
  return (
    <aside
      className="canvas-drawer"
      data-canvas-drawer
      data-mode={mode}
      aria-label={t("Canvas")}
      style={mode === "sheet" ? undefined : { width: w }}
    >
      {mode !== "sheet" ? (
        <div
          className="canvas-resize"
          role="separator"
          aria-orientation="vertical"
          aria-label={t("Resize canvas")}
          aria-valuemin={360}
          aria-valuemax={Math.max(360, Math.floor(viewport * 0.6))}
          aria-valuenow={w}
          tabIndex={0}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onKeyDown={onResizeKey}
        />
      ) : null}
      <header className="canvas-head">
        {mode === "sheet" ? (
          <button type="button" className="canvas-back" onClick={closeDrawer} aria-label={t("Back to chat")}>
            ←
          </button>
        ) : null}
        <div className="canvas-tabs" role="tablist" aria-label={t("Artifacts")}>
          {canvas.tabs.map((x) => (
            <span key={x.id} className="canvas-tab" data-active={x.id === tab.id || undefined}>
              <button
                type="button"
                role="tab"
                aria-selected={x.id === tab.id}
                className="canvas-tab-label"
                onClick={() => focusTab(x.id)}
              >
                {x.title}
              </button>
              <button type="button" className="canvas-tab-close" aria-label={`${t("Close")} ${x.title}`} onClick={() => closeTab(x.id)}>
                <IconClose size={10} />
              </button>
            </span>
          ))}
        </div>
        {mode !== "sheet" ? (
          <button type="button" className="canvas-close" onClick={closeDrawer} aria-label={t("Close canvas")}>
            <IconClose size={12} />
          </button>
        ) : null}
      </header>
      <Body
        key={`${tab.id}@${tab.selected}`}
        conversation={canvas.conversation}
        tab={tab}
        composerKey={canvas.composerKey}
        working={canvas.working}
      />
    </aside>
  );
}
