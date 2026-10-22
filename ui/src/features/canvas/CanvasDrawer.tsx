import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { setComposerRefs } from "../app-shell/chat/composerStore";
import ArtifactView, { downloadName, useArtifactBytes } from "./ArtifactView";
import { KIND_LABEL, sizeLabel } from "./ArtifactCard";
import { makeAskRef } from "./askRefs";
import {
  clampWidth,
  closeDrawer,
  defaultWidth,
  closeTab,
  drawerLayout,
  focusTab,
  readWidth,
  selectVersion,
  useCanvas,
  writeWidth,
  type CanvasTab,
  type DrawerLayout,
  type Surface,
  type Working,
} from "./canvasStore";

/** Versions the store keeps per artifact (the daemon prunes older ones). */
const KEPT_VERSIONS = 20;

const FOOT: Record<string, string[]> = {
  html: ["sandboxed frame", "no scripts · no network"],
  md: ["board Markdown renderer", "no raw HTML"],
  pdf: ["download only"],
  image: ["inline image"],
  csv: ["table · first 2,000 rows"],
};

function useViewport(): number {
  const [w, setW] = useState(() => window.innerWidth);
  useEffect(() => {
    const on = () => setW(window.innerWidth);
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, []);
  return w;
}

/** The chat pane's right edge with no drawer padding applied (app shell). */
function chatRightEdge(pushed: number): number | null {
  const pane = document.querySelector<HTMLElement>("[data-chat-pane]");
  return pane ? pane.getBoundingClientRect().right + pushed : null;
}

/**
 * The running turn from the thread alone. The daemon announces no stage names
 * and no total, so: the agent's latest words, the steps so far (the live one
 * marked), "step N" with no "of M", and an indeterminate bar.
 */
function WorkingStrip({ working, updating }: { working: Working; updating: string | null }) {
  const t = useTranslate();
  return (
    <div className="cv-work" role="status" aria-live="polite" data-canvas-working>
      {working.plan ? <p className="cv-plan">{working.plan}</p> : null}
      {working.steps.length > 0 ? (
        <ol className="cv-stage" aria-label={t("Steps so far")}>
          {working.steps.map((s, i) => (
            <li key={`${i}:${s.text}`} data-s={s.done ? "done" : "now"}>
              {s.text}
            </li>
          ))}
        </ol>
      ) : null}
      <div className="cv-live">
        <span className="workdot live" aria-hidden />
        <span>
          {working.count > 0 ? `${t("step")} ${working.count}` : t("Working")}
          {working.step ? ` · ${working.step}` : ""}
        </span>
        {updating ? <span className="artbadge">{updating}</span> : null}
      </div>
      {working.steps.length > 0 ? (
        <details className="cv-details">
          <summary>{t("details")}</summary>
          <ul>
            {working.steps.map((s, i) => (
              <li key={`${i}:${s.raw}`}>{s.raw}</li>
            ))}
          </ul>
        </details>
      ) : null}
      <div className="cv-bar" aria-hidden>
        <i className="cv-bar-run" />
      </div>
    </div>
  );
}

/** The passage the operator selected inside the document, or "". */
function selectedPassage(root: HTMLElement | null): string {
  const sel = window.getSelection();
  if (!root || !sel || sel.isCollapsed || !sel.anchorNode || !root.contains(sel.anchorNode)) return "";
  return sel.toString().trim().slice(0, 1200);
}

function focusComposer(): void {
  const box = document.querySelector<HTMLElement>("#app-shell-chat-box, [data-composer] textarea");
  box?.focus();
}

function Body({ conversation, tab, composerKey, working, sheet, expanded, onExpand }: {
  conversation: string;
  tab: CanvasTab;
  composerKey: string | null;
  working: Working | null;
  sheet: boolean;
  expanded: boolean;
  onExpand: () => void;
}) {
  const t = useTranslate();
  const state = useArtifactBytes(conversation, tab.id, tab.selected);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const info = tab.versions[tab.selected];
  // "updating v1 → v2" only while this turn has asked to add a version to
  // this artifact and it has not landed; once it has, "updated to v2".
  const putThisTurn = !!working && tab.turn !== null && tab.turn === working.turn;
  const updating = !working
    ? null
    : putThisTurn
      ? tab.latest > 1
        ? `${t("updated to")} v${tab.latest}`
        : null
      : working.updating.includes(tab.id)
        ? `${t("updating")} v${tab.latest} → v${tab.latest + 1}`
        : null;

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

  // Ask to change: a reference chip in the composer, never a send. On a phone
  // the sheet closes so the composer shows.
  const askToChange = () => {
    if (!composerKey) return;
    const passage = selectedPassage(contentRef.current);
    setComposerRefs(composerKey, [makeAskRef({ artifact: tab.id, version: tab.selected, title: tab.title, passage })]);
    if (sheet) closeDrawer();
    requestAnimationFrame(focusComposer);
  };

  const first = Math.max(1, tab.latest - KEPT_VERSIONS + 1);
  const versions: number[] = [];
  for (let v = first; v <= tab.latest; v++) versions.push(v);
  return (
    <>
      <header className="cv-head">
        {sheet ? (
          <button type="button" className="cv-iconbtn cv-back" onClick={closeDrawer} aria-label={t("Back to chat")}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        ) : null}
        <span className="arttype">{KIND_LABEL[tab.kind] ?? "FILE"}</span>
        <span className="cv-title">
          <b>{tab.title}</b>
          <span>
            <span className={`artbadge ${working ? "" : "ok"}`}>{working ? t("drafting") : t("ready")}</span>
            {tab.kind} · v{tab.selected}
          </span>
        </span>
        {!sheet ? (
          <>
            <button
              type="button"
              className="cv-iconbtn"
              onClick={onExpand}
              aria-pressed={expanded}
              aria-label={expanded ? t("Restore canvas width") : t("Expand canvas")}
              title={expanded ? t("Restore canvas width") : t("Expand canvas")}
            >
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                {expanded ? <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /> : <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />}
              </svg>
            </button>
            <button type="button" className="cv-iconbtn" onClick={closeDrawer} aria-label={t("Close canvas")}>
              <IconClose size={12} />
            </button>
          </>
        ) : null}
        <span className="cv-tools">
          <span className="vers" role="group" aria-label={t("Version")}>
            {versions.map((v) => (
              <button key={v} type="button" aria-pressed={v === tab.selected} onClick={() => selectVersion(tab.id, v)}>
                v{v}
              </button>
            ))}
          </span>
          {!sheet ? (
            <>
              <button type="button" className="btn btn-ghost" disabled={!composerKey || !!working} onClick={askToChange}>
                {t("Ask to change")}
              </button>
              <button type="button" className="btn" disabled={!!working} onClick={download}>
                {t("Download")}
              </button>
            </>
          ) : null}
        </span>
        {sheet ? (
          <span className="cv-tools">
            <button type="button" className="btn btn-ghost" disabled={!composerKey || !!working} onClick={askToChange}>
              {t("Ask to change")}
            </button>
            <button type="button" className="btn" disabled={!!working} onClick={download}>
              {t("Download")}
            </button>
          </span>
        ) : null}
      </header>
      {working ? <WorkingStrip working={working} updating={updating} /> : null}
      <div className="cv-body" data-canvas-body>
        {state.status === "loading" ? <p className="text-label text-ink-500 p-3">{t("Loading…")}</p> : null}
        {state.status === "failed" ? <p className="text-label text-fail p-3" role="alert">{state.error}</p> : null}
        {state.status === "ready" ? (
          <ArtifactView kind={tab.kind} title={tab.title} data={state.data} contentRef={contentRef} />
        ) : null}
      </div>
      <footer className="cv-foot">
        <span>
          v{tab.selected} · {sizeLabel(info?.bytes ?? tab.bytes)}
          {info?.digest ? ` · sha256:${info.digest.slice(0, 6)}…${info.digest.slice(-3)}` : ""}
        </span>
        {(FOOT[tab.kind] ?? []).map((f) => (
          <span key={f}>{f}</span>
        ))}
      </footer>
    </>
  );
}

/**
 * The agent canvas (CAD-1726): one non-modal drawer docked to the right,
 * mounted once at the shell. push / cover / sheet by the room the chat keeps
 * (see `drawerLayout`); resizable by pointer and arrow keys; Esc closes; no
 * scrim, no focus trap.
 */
export default function CanvasDrawer() {
  const t = useTranslate();
  const canvas = useCanvas();
  const viewport = useViewport();
  // The width the operator dragged to wins; until then each kind has its own.
  const [dragged, setDragged] = useState(readWidth);
  const [expanded, setExpanded] = useState(false);
  const open = canvas.open && canvas.tabs.length > 0 && canvas.conversation !== null;
  const tab = canvas.tabs.find((x) => x.id === canvas.active) ?? canvas.tabs[0] ?? null;
  const want = clampWidth(dragged ?? defaultWidth(tab?.kind ?? "md", viewport), viewport);
  const [layout, setLayout] = useState<DrawerLayout>({ mode: "push", width: want });
  const pushed = useRef(0);
  const rootRef = useRef<HTMLElement | null>(null);

  // The layout is measured from the page (the chat pane's edge), so it is
  // recomputed after every render that could move it.
  useLayoutEffect(() => {
    if (!open) return;
    const surface: Surface = canvas.surface;
    const next = drawerLayout(viewport, want, surface, surface === "app" ? chatRightEdge(pushed.current) : null, expanded);
    pushed.current = next.mode === "push" ? next.width : 0;
    setLayout((cur) => (cur.mode === next.mode && cur.width === next.width ? cur : next));
  });

  // The page makes room (push) or steps aside through these two attributes.
  useLayoutEffect(() => {
    const root = document.documentElement;
    if (!open) {
      root.removeAttribute("data-canvas");
      root.style.removeProperty("--canvas-w");
      return;
    }
    root.setAttribute("data-canvas", layout.mode);
    root.style.setProperty("--canvas-w", `${layout.width}px`);
    return () => {
      root.removeAttribute("data-canvas");
      root.style.removeProperty("--canvas-w");
    };
  }, [open, layout.mode, layout.width]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      // Esc first restores the width, then closes.
      if (expanded) setExpanded(false);
      else closeDrawer();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, expanded]);

  useEffect(() => {
    if (!open) setExpanded(false);
  }, [open]);

  // The active tab scrolls into view when it changes.
  useEffect(() => {
    rootRef.current?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [canvas.active, canvas.tabs.length, open]);

  const commit = useCallback((px: number) => {
    const next = clampWidth(px, window.innerWidth);
    setDragged(next);
    writeWidth(next);
  }, []);

  const drag = useRef<{ x: number; w: number } | null>(null);
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    drag.current = { x: e.clientX, w: layout.width };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    setDragged(clampWidth(drag.current.w + (drag.current.x - e.clientX), window.innerWidth));
  };
  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current = null;
    e.currentTarget.releasePointerCapture(e.pointerId);
    writeWidth(layout.width);
  };
  const onResizeKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 96 : 24;
    if (e.key === "ArrowLeft") commit(layout.width + step);
    else if (e.key === "ArrowRight") commit(layout.width - step);
    else return;
    e.preventDefault();
  };

  if (!open || !tab || canvas.conversation === null) return null;
  const sheet = layout.mode === "sheet";
  const working = canvas.working;
  return (
    <aside
      ref={rootRef}
      className="canvas-drawer"
      data-canvas-drawer
      data-mode={layout.mode}
      aria-label={t("Canvas")}
      style={sheet ? undefined : { width: layout.width }}
    >
      {!sheet && !expanded ? (
        <div
          className="cv-grip"
          role="separator"
          aria-orientation="vertical"
          aria-label={t("Resize canvas")}
          aria-valuemin={360}
          aria-valuemax={Math.max(360, Math.floor(viewport * 0.6))}
          aria-valuenow={layout.width}
          tabIndex={0}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onKeyDown={onResizeKey}
        />
      ) : null}
      {canvas.tabs.length > 1 ? (
        <div className="cv-tabs" role="tablist" aria-label={t("Artifacts")}>
          {canvas.tabs.map((x) => (
            <span key={x.id} className="cv-tab" data-active={x.id === tab.id || undefined}>
              <button type="button" role="tab" aria-selected={x.id === tab.id} className="cv-tab-label" onClick={() => focusTab(x.id)}>
                {x.title}
              </button>
              <button type="button" className="cv-tab-close" aria-label={`${t("Close")} ${x.title}`} onClick={() => closeTab(x.id)}>
                <IconClose size={10} />
              </button>
            </span>
          ))}
        </div>
      ) : null}
      <Body
        key={`${tab.id}@${tab.selected}`}
        conversation={canvas.conversation}
        tab={tab}
        composerKey={canvas.composerKey}
        working={working}
        sheet={sheet}
        expanded={expanded}
        onExpand={() => setExpanded((x) => !x)}
      />
    </aside>
  );
}
