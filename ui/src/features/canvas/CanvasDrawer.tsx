import { useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, privateReadsAllowed } from "../../lib/privateSession";
import { navigate, useHref } from "../../lib/useLocation";
import { IconClose, IconThreads } from "../../ui/icons";
import { AppGlyph } from "../explorer/shared";
import { WriteGate } from "../auth/WriteGate";
import { requestComposerFocus, setComposerRefs } from "../app-shell/chat/composerStore";
import {
  consumeSaveAsTemplateRequest,
  extractHtmlPlaceholders,
  onSaveAsTemplateRequested,
  parameterizeHtmlForTemplate,
  requestCanvasTemplateReview,
  stageCanvasTemplatePrefill,
} from "../templates/canvasTemplateDraft";
import { templatesApi } from "../templates/templatesApi";
import ArtifactView, { downloadName, htmlFrameDocument, saveBytes, useArtifactBytes, type HtmlView } from "./ArtifactView";
import { KIND_LABEL, sizeLabel } from "./ArtifactCard";
import { makeAskRef } from "./askRefs";
import AppViewBody from "./AppViewBody";
import RecordViewBody from "./RecordViewBody";
import BrowserPanel, { BrowserTabLabel } from "./BrowserPanel";
import { canvasChrome, setCanvasFull, setChatPeek, useCanvasChrome } from "./canvasChrome";
import { effective, MULTI_MIN_WIDTH } from "./canvasLayout";
import LayoutSwitcher from "./LayoutSwitcher";
import PaneStage, { TAB_DRAG_TYPE } from "./PaneStage";
import { closeBrowserDrawer, focusBrowser, hideBrowser, isBrowserPanelShown, tabVisible, useBrowserTab } from "./browserStore";
import {
  clampWidth,
  closeDrawer,
  defaultWidthFor,
  closeTab,
  drawerLayout,
  focusTab,
  readWidth,
  selectVersion,
  setLayoutMode,
  unpane,
  useCanvas,
  writeWidth,
  isArtifactTab,
  type ArtifactTab,
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
  csv: ["table · first 2,000 rows · 50 columns"],
};

const HTML_SOURCE_FOOT = ["source", "read-only"];
const HTML_VIEWS: readonly [HtmlView, string][] = [
  ["rendered", "Rendered"],
  ["source", "HTML"],
];

function useViewport(): number {
  const [w, setW] = useState(() => window.innerWidth);
  useEffect(() => {
    const on = () => setW(window.innerWidth);
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, []);
  return w;
}

/** The stable app workspace box; the assistant lives beyond its right edge. */
function appWorkspaceBounds(): { left: number; right: number; top: number } | null {
  const pane = document.querySelector<HTMLElement>("#app-shell-workspace");
  const bounds = pane?.getBoundingClientRect();
  return bounds && bounds.width > 0 ? { left: bounds.left, right: bounds.right, top: bounds.top } : null;
}

/** The passage the operator selected inside the document, or "". */
function selectedPassage(root: HTMLElement | null): string {
  const sel = window.getSelection();
  if (!root || !sel || sel.isCollapsed || !sel.anchorNode || !root.contains(sel.anchorNode)) return "";
  return sel.toString().trim().slice(0, 1200);
}

function cssEscape(value: string): string {
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") return CSS.escape(value);
  return value.replace(/(["\\])/g, "\\$1");
}

function defaultTemplateSlug(title: string): string {
  const slug = title
    .toLowerCase()
    .replace(/\.[a-z0-9]+$/i, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return slug || "quotation-draft";
}

function focusFallback(fallbackArtifactId?: string | null): void {
  const target = document.querySelector<HTMLElement>("#app-shell-chat-box, [data-composer] textarea")
    ?? (fallbackArtifactId ? document.querySelector<HTMLElement>(`[data-artifact="${cssEscape(fallbackArtifactId)}"]`) : null)
    ?? document.querySelector<HTMLElement>("main button, main a, main input, nav a, button");
  target?.focus();
}

/** A control that takes typed text: Esc leaves it before it leaves anything else. */
function isTextField(el: Element | null): boolean {
  return el instanceof HTMLElement && (el.isContentEditable || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement || el instanceof HTMLInputElement);
}

function Body({ conversation, tab, composerKey, working, sheet, expanded, canExpand, onExpand, onClose, onPaneClose, htmlView, onHtmlView, hosted, templateReviewEnabled }: {
  conversation: string;
  tab: ArtifactTab;
  composerKey: string | null;
  working: Working | null;
  sheet: boolean;
  expanded: boolean;
  canExpand: boolean;
  onExpand: () => void;
  onClose: () => void;
  /** In a pane: the × returns the view to its tab instead of closing the canvas. */
  onPaneClose?: () => void;
  hosted: boolean;
  templateReviewEnabled: boolean;
  htmlView: HtmlView;
  onHtmlView: (view: HtmlView) => void;
}) {
  const t = useTranslate();
  const writeGate = useContext(WriteGate);
  const state = useArtifactBytes(conversation, tab.id, tab.selected);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const info = tab.versions[tab.selected];
  // A version whose size is unknown shows none, never another version's.
  const size = info?.bytes;
  const [saveOpen, setSaveOpen] = useState(false);
  const [tplForm, setTplForm] = useState({ name: "", kind: "quotation" as "quotation" | "invoice", currency: "HKD", taxBps: "0", discountBps: "0", customerLiteral: "", confirmed: false });
  const [tplBusy, setTplBusy] = useState(false);
  const [tplError, setTplError] = useState<string | null>(null);
  const [tplSaved, setTplSaved] = useState<{ name: string; version: number; sha256: string; html: string; specJson: string; sampleHtml?: string } | null>(null);
  const saveLifecycle = useRef(0);
  const patchForm = (patch: Partial<typeof tplForm>) => setTplForm((f) => ({ ...f, ...patch }));

  useEffect(() => {
    const reset = () => { saveLifecycle.current += 1; setSaveOpen(false); setTplBusy(false); setTplError(null); setTplSaved(null); };
    // Body is keyed by artifact version. Reset on revocation, not on effect
    // replay: replay would clear a Save as Template request just consumed.
    const unsubscribe = onPrivateReadsPaused(reset);
    return () => {
      saveLifecycle.current += 1;
      unsubscribe();
    };
  }, []);

  useEffect(() => {
    if (tab.kind !== "html") return;
    const maybeOpen = () => {
      if (!consumeSaveAsTemplateRequest(tab.id)) return;
      setTplError(null);
      setTplSaved(null);
      setTplForm((f) => ({ ...f, name: f.name.trim() ? f.name : defaultTemplateSlug(tab.title) }));
      setSaveOpen(true);
    };
    maybeOpen();
    return onSaveAsTemplateRequested(maybeOpen);
  }, [tab.id, tab.title, tab.kind]);

  const htmlSourceText = tab.kind === "html" && state.status === "ready" ? new TextDecoder().decode(state.data.bytes) : "";
  const detectedPlaceholders = tab.kind === "html" && saveOpen && htmlSourceText ? extractHtmlPlaceholders(htmlSourceText) : null;

  const submitSaveTemplate = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!privateReadsAllowed() || !htmlSourceText || writeGate !== null) return;
    const cleanName = tplForm.name.trim().toLowerCase();
    if (!cleanName) return setTplError(t("Template name is required."));
    const taxRateBps = Number.parseInt(tplForm.taxBps.trim() || "0", 10);
    const discountBps = Number.parseInt(tplForm.discountBps.trim() || "0", 10);
    if (!Number.isFinite(taxRateBps) || taxRateBps < 0 || !Number.isFinite(discountBps) || discountBps < 0) {
      return setTplError(t("Tax and discount bps must be non-negative integers."));
    }
    const prepared = parameterizeHtmlForTemplate(htmlSourceText, { customerLiteral: tplForm.customerLiteral });
    const spec = {
      kind: tplForm.kind,
      fields: prepared.scalars.map((key) => ({ key, label: key, kind: "text", required: true })),
      fixed_text: {},
      calculation: { confirmed: tplForm.confirmed, currency: tplForm.currency.trim() || "HKD", tax_rate_bps: taxRateBps, discount_bps: discountBps },
    };
    const token = saveLifecycle.current;
    setTplBusy(true);
    setTplError(null);
    try {
      const created = await templatesApi.createHtml(cleanName, prepared.html, spec);
      if (token !== saveLifecycle.current || !privateReadsAllowed()) return;
      let sampleHtml: string | undefined;
      if (!hosted) {
        try {
          const s = await templatesApi.sample(created.template.name, created.template.version, "html");
          if (token !== saveLifecycle.current || !privateReadsAllowed()) return;
          sampleHtml = s.html ?? (s.html_base64 ? new TextDecoder().decode(Uint8Array.from(atob(s.html_base64), (c) => c.charCodeAt(0))) : undefined);
        } catch {}
      }
      setTplSaved({ name: created.template.name, version: created.template.version, sha256: created.template.content_sha256, html: prepared.html, specJson: JSON.stringify(spec, null, 2), sampleHtml });
    } catch (cause) {
      if (token === saveLifecycle.current && privateReadsAllowed()) setTplError(cause instanceof Error ? cause.message : t("Couldn’t save template."));
    } finally {
      if (token === saveLifecycle.current) setTplBusy(false);
    }
  };
  // "updating v1 → v2" only while this turn has asked to add a version to
  // this artifact and it has not landed; once it has, "updated to v2".
  // A put still waiting for its result wins, so a second put of the same
  // artifact in one turn shows its own chip.
  const putThisTurn = !!working && tab.turn !== null && tab.turn === working.turn;
  // Only a put waiting on this artifact touches it: any other running turn
  // leaves this document finished, downloadable and open to a change request.
  const changing = !!working && working.updating.includes(tab.id);
  const updating = !working
    ? null
    : working.updating.includes(tab.id)
      ? `${t("updating")} v${tab.latest} → v${tab.latest + 1}`
      : putThisTurn && tab.latest > 1
        ? `${t("updated to")} v${tab.latest}`
        : null;

  const download = () => {
    if (state.status !== "ready") return;
    saveBytes(state.data.bytes, state.data.mime, downloadName(tab.title, tab.kind, state.data.mime));
  };

  // Ask to change: a reference chip in the composer, never a send. On a phone
  // the sheet closes so the composer shows.
  const askToChange = () => {
    if (!composerKey) return;
    const passage = selectedPassage(contentRef.current);
    setComposerRefs(composerKey, [makeAskRef({ artifact: tab.id, version: tab.selected, title: tab.title, passage })]);
    requestComposerFocus(composerKey);
    if (sheet) onClose();
  };

  const first = Math.max(1, tab.latest - KEPT_VERSIONS + 1);
  const versions: number[] = [];
  for (let v = first; v <= tab.latest; v++) versions.push(v);
  // An image is asked about in the chat and downloaded from its own overlay,
  // so its header keeps no Ask to change or Download, its footer no hash, and
  // its version picker only a choice between versions.
  const image = tab.kind === "image";
  const showTools = !image || versions.length > 1;
  const saveAsTemplateButton = tab.kind === "html" && state.status === "ready" ? (
    <button
      type="button"
      className="btn btn-ghost"
      disabled={changing || writeGate !== null}
      onClick={() => {
        setTplForm((f) => ({ ...f, name: f.name.trim() ? f.name : defaultTemplateSlug(tab.title) }));
        setSaveOpen((open) => !open);
      }}
    >
      {t("Save as Template")}
    </button>
  ) : null;
  return (
    <>
      <header className="cv-head">
        {sheet ? (
          <button type="button" className="cv-iconbtn cv-back" onClick={onClose} aria-label={t("Back to chat")}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        ) : null}
        <span className="arttype">{KIND_LABEL[tab.kind] ?? "FILE"}</span>
        <span className="cv-title">
          <b>{tab.title}</b>
          <span>
            <span className={`artbadge ${changing ? "" : "ok"}`} role="status">{changing ? t("drafting") : t("ready")}</span>
            {updating ? <span className="artbadge">{updating}</span> : null}
            {tab.kind} · v{tab.selected}
          </span>
        </span>
        {!sheet ? (
          <>
            <button
              type="button"
              className="cv-iconbtn"
              onClick={onExpand}
              disabled={!canExpand}
              aria-pressed={expanded}
              aria-label={t("Expand canvas")}
              title={t("Expand canvas")}
            >
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                {expanded ? <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /> : <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />}
              </svg>
            </button>
            <button type="button" className="cv-iconbtn" onClick={onPaneClose ?? onClose} aria-label={t(onPaneClose ? "Close pane" : "Close canvas")} title={onPaneClose ? t("Close pane — the view stays in the tabs") : undefined}>
              <IconClose size={12} />
            </button>
          </>
        ) : null}
        {showTools ? (
        <span className="cv-tools">
          <span className="cv-pick">
            <span className="vers" role="group" aria-label={t("Version")}>
              {versions.map((v) => (
                <button key={v} type="button" aria-pressed={v === tab.selected} onClick={() => selectVersion(tab.id, v)}>
                  v{v}
                </button>
              ))}
            </span>
            {tab.kind === "html" ? (
              <span className="vers cv-view" role="group" aria-label={t("View")}>
                {HTML_VIEWS.map(([view, label]) => (
                  <button
                    key={view}
                    type="button"
                    aria-pressed={view === htmlView}
                    disabled={state.status !== "ready"}
                    onClick={() => onHtmlView(view)}
                  >
                    {t(label)}
                  </button>
                ))}
              </span>
            ) : null}
          </span>
          {!sheet && !image ? (
            <>
              <button type="button" className="btn btn-ghost" disabled={!composerKey || changing} onClick={askToChange}>
                {t("Ask to change")}
              </button>
              {saveAsTemplateButton}
              <button type="button" className="btn" disabled={changing} onClick={download}>
                {t("Download")}
              </button>
            </>
          ) : null}
        </span>
        ) : null}
        {sheet && !image ? (
          <span className="cv-tools">
            <button type="button" className="btn btn-ghost" disabled={!composerKey || changing} onClick={askToChange}>
              {t("Ask to change")}
            </button>
            {saveAsTemplateButton}
            <button type="button" className="btn" disabled={changing} onClick={download}>
              {t("Download")}
            </button>
          </span>
        ) : null}
        {changing ? (
          <div className="cv-bar" aria-hidden data-canvas-working>
            <i className="cv-bar-run" />
          </div>
        ) : null}
      </header>
      {saveOpen && tab.kind === "html" && state.status === "ready" ? (
        <form className="p-3 border-b border-ink-800 bg-ink-950/60 grid gap-2 text-label min-w-0 overflow-x-hidden" onSubmit={submitSaveTemplate} data-save-as-template>
          <div className="flex items-center justify-between gap-2 min-w-0">
            <strong>{t("Save as Template")}</strong>
            <button type="button" className="btn btn-ghost" onClick={() => setSaveOpen(false)}>{t("Cancel")}</button>
          </div>
          {detectedPlaceholders && (detectedPlaceholders.scalars.length > 0 || detectedPlaceholders.itemFields.length > 0) ? (
            <div className="flex flex-wrap gap-1 min-w-0">
              {detectedPlaceholders.scalars.map((k) => <code key={`s-${k}`} className="artbadge">{`{{ ${k} }}`}</code>)}
              {detectedPlaceholders.itemFields.map((k) => <code key={`i-${k}`} className="artbadge">{`{{ item.${k} }}`}</code>)}
            </div>
          ) : <p className="text-micro text-ink-400 m-0" data-auto-param-note>{t("No {{ field }} placeholders found — a {{ customer }} slot will be added automatically when saved.")}</p>}
          <div className={`grid gap-2 min-w-0 ${sheet ? "grid-cols-1" : "grid-cols-2"}`}>
            {([["Name", "name", "quotation-acme"], ["Currency", "currency", "HKD"]] as const).map(([lbl, key, ph]) => (
              <label key={key} className="grid gap-1 min-w-0"><span>{t(lbl)}</span><input className="min-w-0 w-full" value={tplForm[key]} onChange={(e) => patchForm({ [key]: e.target.value })} placeholder={ph} /></label>
            ))}
            <label className="grid gap-1 min-w-0"><span>{t("Kind")}</span><select className="min-w-0 w-full" value={tplForm.kind} onChange={(e) => patchForm({ kind: e.target.value as "quotation" | "invoice" })}><option value="quotation">{t("Quotation")}</option><option value="invoice">{t("Invoice")}</option></select></label>
            <div className="grid grid-cols-2 gap-1 min-w-0">
              {([["Tax bps", "taxBps"], ["Discount bps", "discountBps"]] as const).map(([lbl, key]) => (
                <label key={key} className="grid gap-1 min-w-0"><span>{t(lbl)}</span><input className="min-w-0 w-full" value={tplForm[key]} onChange={(e) => patchForm({ [key]: e.target.value })} /></label>
              ))}
            </div>
          </div>
          <label className="grid gap-1 min-w-0"><span>{t("Replace customer text (optional)")}</span><input className="min-w-0 w-full" value={tplForm.customerLiteral} onChange={(e) => patchForm({ customerLiteral: e.target.value })} placeholder="Northwind Ltd" /></label>
          <label className="flex items-center gap-2 min-w-0"><input type="checkbox" checked={tplForm.confirmed} onChange={(e) => patchForm({ confirmed: e.target.checked })} /><span>{t("I confirm the calculation rules above")}</span></label>
          {tplError && <p role="alert" className="text-fail break-words">{tplError}</p>}
          {tplSaved && (
            <div className="grid gap-2 min-w-0">
              <p role="status" className="artbadge ok break-all">{t("Saved")} {tplSaved.name} v{tplSaved.version} ({t("draft")}) · sha256:{tplSaved.sha256}</p>
              {tplSaved.sampleHtml && (
                <div className="grid gap-1 min-w-0" data-canvas-sample-preview>
                  <p className="text-micro text-ink-300 m-0">{t("SAMPLE: placeholder values only. This is not a document to send.")}</p>
                  <iframe className="w-full h-44 rounded border border-ink-800 bg-white" sandbox="" referrerPolicy="no-referrer" srcDoc={htmlFrameDocument(tplSaved.sampleHtml)} title={`SAMPLE HTML of ${tplSaved.name} v${tplSaved.version}`} />
                </div>
              )}
              <div className="flex justify-end">{templateReviewEnabled ? (
                <button type="button" className="btn btn-ghost" onClick={(event) => {
                  requestCanvasTemplateReview({ name: tplSaved.name, version: tplSaved.version, digest: tplSaved.sha256 }, event.currentTarget);
                  if (window.location.pathname !== "/") navigate("/");
                }}>{t("Review template")}</button>
              ) : !hosted ? (
                <button type="button" className="btn btn-ghost" data-open-in-templates onClick={() => { stageCanvasTemplatePrefill({ name: tplSaved.name, html: tplSaved.html, specJson: tplSaved.specJson }); onClose(); navigate("/settings/templates"); }}>{t("Open in Templates")}</button>
              ) : null}</div>
            </div>
          )}
          <div className="flex justify-end gap-2"><button type="submit" className="btn" disabled={tplBusy || writeGate !== null || !tplForm.name.trim() || !tplForm.confirmed}>{tplBusy ? t("Saving…") : t("Save draft v1")}</button></div>
        </form>
      ) : null}
      <div className="cv-body" data-canvas-body>
        {state.status === "loading" ? <p className="text-label text-ink-500 p-3">{t("Loading…")}</p> : null}
        {state.status === "failed" ? <p className="text-label text-fail p-3" role="alert">{state.error}</p> : null}
        {state.status === "ready" ? (
          <ArtifactView kind={tab.kind} title={tab.title} data={state.data} contentRef={contentRef} htmlView={htmlView} aspect={tab.aspect} />
        ) : null}
      </div>
      {image ? null : (
      <footer className="cv-foot">
        <span>
          v{tab.selected}{size === undefined ? "" : ` · ${sizeLabel(size)}`}
          {info?.digest ? ` · sha256:${info.digest.slice(0, 6)}…${info.digest.slice(-3)}` : ""}
        </span>
        {(tab.kind === "html" && htmlView === "source" ? HTML_SOURCE_FOOT : (FOOT[tab.kind] ?? [])).map((f) => (
          <span key={f}>{f}</span>
        ))}
      </footer>
      )}
    </>
  );
}

/**
 * The agent canvas (CAD-1726): one non-modal drawer docked to the right,
 * mounted once at the shell. push / cover / sheet by the room the chat keeps
 * (see `drawerLayout`); resizable by pointer and arrow keys; Esc closes; no
 * scrim, no focus trap.
 */
export default function CanvasDrawer({ hosted = false, templateReviewEnabled = false }: { hosted?: boolean; templateReviewEnabled?: boolean }) {
  const t = useTranslate();
  const canvas = useCanvas();
  const browser = useBrowserTab();
  const viewport = useViewport();
  // The width the operator dragged to wins; until then each kind has its own.
  const [dragWidth, setDragWidth] = useState(readWidth);
  const [expanded, setExpanded] = useState(false);
  const [canExpand, setCanExpand] = useState(false);
  // The canvas can be as wide as panes need (it asks for that width itself once panes are chosen); the layout switcher is off where it cannot.
  const [fits, setFits] = useState(false);
  // Expanding would make it wide enough (the "panes saved" chip then offers that).
  const [expandFits, setExpandFits] = useState(false);
  // A tab being dragged by its strip entry lights the stage's drop zones.
  const [dragged, setDragged] = useState<string | null>(null);
  // CAD-2690: the tab bar's right end, where an app view docks its actions.
  const [tabActions, setTabActions] = useState<HTMLElement | null>(null);
  const chrome = useCanvasChrome();
  const railRef = useRef<HTMLButtonElement | null>(null);
  const pathname = useHref().split("?")[0];
  // Per artifact, held here because Body remounts when the version changes.
  const [htmlViews, setHtmlViews] = useState<Record<string, HtmlView>>({});
  // The session tab (browserStore.ts) is not an artifact: it can open the drawer
  // on its own and shares the strip, but canvasStore never models it.
  const hasBrowser = tabVisible(browser);
  const conversation = canvas.conversation ?? browser.conversation;
  const open = conversation !== null && ((canvas.open && canvas.tabs.length > 0) || (browser.open && hasBrowser));
  const tab = canvas.tabs.find((x) => x.id === canvas.active) ?? canvas.tabs[0] ?? null;
  const showBrowser = isBrowserPanelShown(canvas, browser);
  const rootRef = useRef<HTMLElement | null>(null);
  const lastArtifactIdRef = useRef<string | null>(tab?.id ?? null);
  lastArtifactIdRef.current = tab?.id ?? lastArtifactIdRef.current;
  // Where focus goes when a control inside the drawer goes away (a closed or
  // hidden tab, or the drawer itself): the next tab, else the drawer; if the
  // drawer is gone, the composer (or the triggering artifact card). Only if
  // focus was in the drawer or nowhere: a person who is somewhere else keeps it.
  const handFocusOn = useCallback(() => {
    const at = document.activeElement;
    const mine = at === document.body || !!rootRef.current?.contains(at);
    const fallbackId = tab?.id ?? lastArtifactIdRef.current;
    requestAnimationFrame(() => {
      if (!mine) return;
      const current = document.activeElement;
      if (current !== at && current !== document.body && !rootRef.current?.contains(current)) return;
      const root = rootRef.current;
      if (!root) {
        if (canvas.composerKey !== null) {
          requestComposerFocus(canvas.composerKey);
          return;
        }
        focusFallback(fallbackId);
        return;
      }
      (root.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]') ?? root).focus();
    });
  }, [canvas.composerKey]);
  const closeAll = useCallback(() => {
    handFocusOn();
    closeDrawer();
    closeBrowserDrawer();
  }, [handFocusOn]);
  const baseWant = dragWidth !== null ? clampWidth(dragWidth, viewport) : defaultWidthFor(showBrowser ? "html" : (tab?.kind ?? "md"), viewport);
  const saved = effective(canvas.layout, canvas.tabs.map((x) => x.id));
  const [layout, setLayout] = useState<DrawerLayout>({ mode: "push", width: baseWant });
  const [canvasTop, setCanvasTop] = useState(0);
  const [, measureWorkspace] = useState(0);

  // A chat toggle changes the available workspace without remounting it.
  useLayoutEffect(() => {
    if (!open || canvas.surface !== "app") return;
    const workspace = document.querySelector("#app-shell-workspace");
    if (!workspace) return;
    const observer = new ResizeObserver(() => measureWorkspace((value) => value + 1));
    observer.observe(workspace);
    return () => observer.disconnect();
  }, [open, canvas.surface]);

  // Measure after layout; cover mode leaves both underlying panes unchanged.
  useLayoutEffect(() => {
    if (!open) return;
    const surface: Surface = canvas.surface;
    const workspace = surface === "app" ? appWorkspaceBounds() : null;
    // Saved panes ask for the width they need, but only where that width is really there: otherwise the canvas stays as it was.
    const asked = drawerLayout(viewport, Math.max(baseWant, MULTI_MIN_WIDTH), surface, workspace, expanded);
    const roomy = asked.mode !== "sheet" && asked.width >= MULTI_MIN_WIDTH;
    const wanted = saved.panes.length >= 2 && roomy ? Math.max(baseWant, MULTI_MIN_WIDTH) : baseWant;
    const next = drawerLayout(viewport, wanted, surface, workspace, expanded);
    setLayout((cur) => (cur.mode === next.mode && cur.width === next.width && cur.right === next.right && cur.full === next.full ? cur : next));
    setCanvasTop(surface === "app" && next.mode === "cover" && !next.full && workspace !== null ? Math.max(0, workspace.top) : 0);
    const widest = drawerLayout(viewport, wanted, surface, workspace, true);
    setCanExpand(expanded || widest.width > next.width);
    setExpandFits(widest.mode !== "sheet" && widest.width >= MULTI_MIN_WIDTH);
    setFits(roomy);
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

  // The board makes room for a full-width canvas: its sidebar folds, the chat steps out of sight.
  const full = open && layout.full === true;
  useEffect(() => {
    setCanvasFull(full);
    return () => setCanvasFull(false);
  }, [full]);
  useLayoutEffect(() => {
    const root = document.documentElement;
    if (full) root.setAttribute("data-canvas-full", "");
    if (full && chrome.peek) root.setAttribute("data-canvas-peek", "");
    return () => {
      root.removeAttribute("data-canvas-full");
      root.removeAttribute("data-canvas-peek");
    };
  }, [full, chrome.peek]);
  useEffect(() => {
    if (chrome.peek && canvas.composerKey !== null) requestComposerFocus(canvas.composerKey);
  }, [chrome.peek, canvas.composerKey]);
  // The peek is a drawer over the canvas: leaving the chat for the canvas (a click or the frame taking focus) shuts it.
  useEffect(() => {
    if (!chrome.peek) return;
    const away = (e: PointerEvent) => {
      const target = e.target as Element | null;
      if (target?.closest(".home-workspace, .cv-rail")) return;
      setChatPeek(false);
    };
    const leave = () => setChatPeek(false);
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("blur", leave);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("blur", leave);
    };
  }, [chrome.peek]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (!expanded) {
        closeAll();
        return;
      }
      // Expanded: Esc leaves a focused field, then shuts the chat peek, then restores the split.
      if (isTextField(document.activeElement)) {
        (document.activeElement as HTMLElement).blur();
        e.preventDefault();
      } else if (canvasChrome().peek) setChatPeek(false);
      else setExpanded(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, expanded, closeAll]);

  useEffect(() => {
    if (!open) setExpanded(false);
  }, [open]);
  // Moving to another page puts the board back as it was.
  useEffect(() => setExpanded(false), [pathname]);

  // The active tab stays in view, clear of the Browser tab pinned at the
  // strip's right edge (CAD-2682): when it changes, and whenever the strip
  // resizes (viewport, layout switch, first paint).
  useEffect(() => {
    const strip = rootRef.current?.querySelector<HTMLElement>(".cv-tabs");
    if (!strip) return;
    const reveal = () => {
      const tab = strip.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.closest<HTMLElement>(".cv-tab");
      if (!tab) return;
      tab.scrollIntoView({ block: "nearest", inline: "nearest" });
      const pinned = strip.querySelector<HTMLElement>('.cv-tab[data-kind="session"]');
      if (!pinned || pinned === tab) return;
      const hidden = tab.getBoundingClientRect().right - pinned.getBoundingClientRect().left;
      if (hidden > 0) strip.scrollLeft += hidden;
    };
    reveal();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(reveal);
    observer.observe(strip);
    return () => observer.disconnect();
  }, [canvas.active, canvas.tabs.length, showBrowser, open]);

  const commit = useCallback((px: number) => {
    const next = clampWidth(px, window.innerWidth);
    setDragWidth(next);
    writeWidth(next);
  }, []);

  const drag = useRef<{ x: number; w: number } | null>(null);
  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    drag.current = { x: e.clientX, w: layout.width };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    setDragWidth(clampWidth(drag.current.w + (drag.current.x - e.clientX), window.innerWidth));
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
  const onTabListKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement | null;
    if (target?.getAttribute("role") !== "tab") return;
    const tabs = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
    const idx = tabs.indexOf(target as HTMLButtonElement);
    if (idx < 0 || tabs.length < 2) return;
    let nextIdx = -1;
    if (e.key === "ArrowRight") nextIdx = (idx + 1) % tabs.length;
    else if (e.key === "ArrowLeft") nextIdx = (idx - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") nextIdx = 0;
    else if (e.key === "End") nextIdx = tabs.length - 1;
    if (nextIdx < 0) return;
    e.preventDefault();
    const next = tabs[nextIdx];
    next.click();
    next.focus();
  };

  if (!open || (!tab && !showBrowser) || conversation === null) return null;
  const sheet = layout.mode === "sheet";
  // Narrower than this, or on a phone sheet, panes fall back to tabs; the arrangement stays saved.
  const multi = !sheet && layout.width >= MULTI_MIN_WIDTH;
  // Where panes can be had: now, or by the canvas asking for the width (a layout choice or a drop does).
  const canSplit = !sheet && (multi || fits);

  const working = canvas.working;
  const tabbed = canvas.tabs.length + (hasBrowser ? 1 : 0) > 1;
  const toggleChat = () => {
    setChatPeek(!chrome.peek);
    if (chrome.peek) railRef.current?.focus();
  };
  return (
    <aside
      ref={rootRef}
      className="canvas-drawer"
      data-canvas-drawer
      data-mode={layout.mode}
      data-surface={canvas.surface}
      aria-label={t("Canvas")}
      tabIndex={-1}
      data-full={layout.full || undefined}
      style={sheet ? undefined : { width: layout.width, right: layout.right ?? 0, top: canvasTop }}
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
      {layout.full ? (
        <div className="cv-rail" data-chat-rail>
          <button ref={railRef} type="button" className="cv-iconbtn cv-rail-chat" aria-label={t("Assistant chat")} title={t("Assistant chat")} aria-expanded={chrome.peek} onClick={toggleChat}>
            <IconThreads size={16} />
            {chrome.unread ? <span className="cv-dot" role="status" aria-label={t("New reply waiting")} /> : null}
          </button>
        </div>
      ) : null}
      <div className="cv-main">
      {tabbed ? (
        <div className="cv-tabrow">
        <div className="cv-tabs" role="tablist" aria-label={t("Artifacts")} onKeyDown={onTabListKeyDown}>
          {canvas.tabs.map((x) => {
            const on = !showBrowser && tab !== null && x.id === tab.id;
            return (
              <span
                key={x.id}
                className="cv-tab"
                data-active={on || undefined}
                data-paned={(multi && saved.panes.includes(x.id)) || undefined}
                draggable={canSplit}
                onDragStart={(e) => {
                  e.dataTransfer.setData(TAB_DRAG_TYPE, x.id);
                  e.dataTransfer.setData("text/plain", x.title);
                  e.dataTransfer.effectAllowed = "move";
                  setDragged(x.id);
                }}
                onDragEnd={() => setDragged(null)}
              >
                <button type="button" role="tab" aria-selected={on} className="cv-tab-label" onClick={() => focusTab(x.id)}>
                  {x.app ? <span className="cv-mark"><AppGlyph name={x.app.appLabel ?? x.app.tag} size="xs" /></span> : null}
                  <span className="cv-tab-title">{x.title}</span>
                </button>
                <button type="button" className="cv-tab-close" aria-label={`${t("Close")} ${x.title}`} onClick={() => {
                    handFocusOn();
                    closeTab(x.id);
                  }}>
                  <IconClose size={10} />
                </button>
              </span>
            );
          })}
          {hasBrowser ? (
            <span className="cv-tab" data-kind="session" data-active={showBrowser || undefined}>
              <button type="button" role="tab" id="bz-tab" aria-selected={showBrowser} aria-controls="bz-panel" className="cv-tab-label" onClick={focusBrowser}>
                <BrowserTabLabel />
              </button>
              <button
                type="button"
                className="cv-tab-close"
                aria-label={t("Hide Browser tab. The session keeps running.")}
                title={t("Hide Browser tab. The session keeps running.")}
                onClick={() => {
                  handFocusOn();
                  hideBrowser();
                }}
              >
                <IconClose size={10} />
              </button>
            </span>
          ) : null}
        </div>
        {canvas.tabs.length >= 2 ? (
          <LayoutSwitcher
            mode={multi ? saved.mode : "tabs"}
            enabled={canSplit}
            saved={multi ? 0 : saved.panes.length}
            canExpand={canExpand && !expanded && expandFits}
            onChoose={setLayoutMode}
            onExpand={() => setExpanded(true)}
          />
        ) : null}
        <div className="cv-tabactions" ref={setTabActions} />
        </div>
      ) : null}
      {tab ? (
        // Kept mounted while the Browser tab is in front, so an artifact's
        // scroll position and frame survive the switch.
        <PaneStage
          tabs={canvas.tabs}
          active={tab.id}
          layout={canvas.layout}
          multi={multi}
          droppable={canSplit}
          hidden={showBrowser}
          dragged={dragged}
          onDragDone={() => setDragged(null)}
          renderBody={(x, paned) => {
            const paneClose = paned ? () => unpane(x.id) : undefined;
            return isArtifactTab(x) ? (
              <Body
                key={`${x.id}@${x.selected}`}
                conversation={canvas.conversation ?? conversation}
                tab={x}
                composerKey={canvas.composerKey}
                working={working}
                sheet={sheet}
                expanded={expanded}
                canExpand={canExpand}
                onExpand={() => setExpanded((v) => !v)}
                onClose={closeAll}
                onPaneClose={paneClose}
                hosted={hosted}
                templateReviewEnabled={templateReviewEnabled}
                htmlView={htmlViews[x.id] ?? "rendered"}
                onHtmlView={(view) => setHtmlViews((views) => ({ ...views, [x.id]: view }))}
              />
            ) : x.app ? (
              <AppViewBody
                key={x.id}
                tab={x}
                app={x.app}
                sheet={sheet}
                tabbed={tabbed}
                tabRow={showBrowser ? null : tabActions}
                expanded={expanded}
                canExpand={canExpand}
                onExpand={() => setExpanded((v) => !v)}
                onClose={closeAll}
                onPaneClose={paneClose}
              />
            ) : x.record ? (
              <RecordViewBody
                key={x.id}
                tab={x}
                view={x.record}
                composerKey={canvas.composerKey}
                sheet={sheet}
                expanded={expanded}
                canExpand={canExpand}
                onExpand={() => setExpanded((v) => !v)}
                onClose={closeAll}
                onPaneClose={paneClose}
              />
            ) : null;
          }}
        />
      ) : null}
      {hasBrowser ? (
        <BrowserPanel
          key={browser.conversation}
          conversation={browser.conversation ?? conversation}
          active={showBrowser}
          sheet={sheet}
          expanded={expanded}
          canExpand={canExpand}
          onExpand={() => setExpanded((x) => !x)}
          onClose={closeAll}
        />
      ) : null}
      </div>
    </aside>
  );
}
