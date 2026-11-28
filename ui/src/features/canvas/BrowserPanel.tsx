import { useEffect, useLayoutEffect, useRef, useState, type FocusEvent } from "react";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { useCanvas } from "./canvasStore";
import { EmptyState } from "../app-shell/shared/States";
import { previewBrowser, viewBrowser, type BrowserPreview } from "./browserClient";
import {
  alertBrowser,
  browserSnapshot,
  cancelStop,
  closeBrowserDrawer,
  confirmStop,
  isBrowserPanelShown,
  needsStopConfirm,
  openBrowser,
  refreshBrowser,
  requestStop,
  startBrowserSession,
  takeoverBrowserSession,
  useBrowserTab,
  type BrowserTabState,
} from "./browserStore";
import { TurnCard } from "../../ui/TurnCard";

/** How long a live view may take to load before it is given up on. */
const FRAME_WAIT_MS = 20000;

export function GlobeIcon({ size = 14 }: { size?: number }) {
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18M12 3c3 3.2 3 14.8 0 18M12 3c-3 3.2-3 14.8 0 18" />
    </svg>
  );
}

type Tone = "accent" | "warn" | "muted" | "fail";
export interface BrowserStatus {
  text: string;
  tone: Tone;
  pulse: boolean;
}

/** The one word for the tab, header and card. */
export function browserStatus(s: BrowserTabState, t: (english: string) => string): BrowserStatus {
  if (s.stop === "stopping") return { text: t("Stopping…"), tone: "warn", pulse: true };
  if (s.loadError) return { text: t("Can't load"), tone: "fail", pulse: false };
  switch (s.session?.state) {
    case "active":
      return {
        text: s.session.signedIn ? t("Reading · Read-only") : t("Reading"),
        tone: "accent",
        pulse: true,
      };
    case "handoff":
      return { text: t("Needs you"), tone: "warn", pulse: true };
    case "ended":
      return { text: t("Ended"), tone: "muted", pulse: false };
    default:
      return { text: t("No session"), tone: "muted", pulse: false };
  }
}

const TONE: Record<Tone, string> = {
  accent: "var(--color-accent)",
  warn: "var(--color-warn)",
  muted: "var(--color-ink-500)",
  fail: "var(--color-fail)",
};

/** The tab's label: icon, name, a state dot, and the state in words for a
 *  screen reader. */
export function BrowserTabLabel() {
  const t = useTranslate();
  const b = useBrowserTab();
  const st = browserStatus(b, t);
  return (
    <>
      <GlobeIcon />
      <span>{t("Browser")}</span>
      <span className="bz-dot" style={{ "--c": TONE[st.tone] } as React.CSSProperties} data-pulse={st.pulse || undefined} aria-hidden />
      <span className="sr-only">{st.text}</span>
    </>
  );
}

/** Split an address for display only: scheme, real host, the rest. The host is
 *  what the URL parser reports (punycode stays punycode, never the userinfo a
 *  link can hide a look-alike host behind); nothing is matched against it. An
 *  address that does not parse is shown whole. */
function addressParts(address: string): { scheme: string; host: string; rest: string } | null {
  try {
    const u = new URL(address);
    if (!u.host) return null;
    return { scheme: `${u.protocol}//`, host: u.host, rest: `${u.pathname}${u.search}` };
  } catch {
    return null;
  }
}

/** The last page the assistant read, in the header and outside the live view
 *  only: inside it the provider's own address bar is the one that matters.
 *  One line. The path gives way first. Inline in the first row only while the
 *  whole host fits there (`hostFitsInRow`); otherwise on its own row under the
 *  header, where a host that still does not fit is clipped from its START, so
 *  the registrable end stays visible. */
function Address({ url }: { url: string }) {
  const t = useTranslate();
  const parts = addressParts(url);
  const displayHost = parts ? (parts.scheme === "https://" ? parts.host : `${parts.scheme}${parts.host}`) : "";
  return (
    <div className="bz-addr" aria-label={t("Page address")} title={parts ? `${parts.scheme}${parts.host}${parts.rest}` : url}>
      {parts ? (
        <>
          <b className="bz-host"><bdi>{displayHost}</bdi></b>
          {parts.rest !== "/" ? <span className="bz-rest">{parts.rest}</span> : null}
        </>
      ) : (
        <span className="bz-rest"><bdi>{url}</bdi></span>
      )}
    </div>
  );
}

/** Whether the address's whole host fits in the room the header's first row
 *  leaves it: the row's width less every other item on it and the gaps. That
 *  room does not depend on where the address sits, so moving it to its own row
 *  cannot flip the answer back. During a stop confirm the actions have their own
 *  row and are not counted. Widths are fractional, with a pixel to spare, so a
 *  rounded width never lets a host that is a fraction too wide stay and clip.
 *  An address that is not shown (a phone's sheet), or no layout at all (a test
 *  DOM), needs no row: no decision. */
function hostFitsInRow(head: HTMLElement): boolean {
  const items = Array.from(head.children) as HTMLElement[];
  const addr = items.find((el) => el.classList.contains("bz-addr"));
  const host = addr?.querySelector<HTMLElement>(".bz-host");
  if (!addr || !host || head.clientWidth === 0 || getComputedStyle(addr).display === "none") return true;
  const cs = getComputedStyle(head);
  const gap = parseFloat(cs.columnGap) || 0;
  let room = head.getBoundingClientRect().width - (parseFloat(cs.paddingLeft) || 0) - (parseFloat(cs.paddingRight) || 0)
    - (parseFloat(cs.borderLeftWidth) || 0) - (parseFloat(cs.borderRightWidth) || 0);
  const confirm = head.hasAttribute("data-confirm");
  for (const el of items) {
    if (el === addr || el.classList.contains("cv-bar") || (confirm && el.classList.contains("bz-actions"))) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.position === "absolute") continue;
    room -= el.getBoundingClientRect().width + gap;
  }
  // The host's text box keeps its full width even while its box clips it.
  const text = (host.firstElementChild as HTMLElement | null) ?? host;
  return text.getBoundingClientRect().width + 1 <= room;
}

const Icon = ({ d, children }: { d?: string; children?: React.ReactNode }) => (
  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    {d ? <path d={d} /> : children}
  </svg>
);

type ViewerState =
  | { phase: "idle" }
  | { phase: "connecting" }
  | {
      phase: "loading" | "open";
      url: string;
      n: number;
      /** True once this open frame stayed read-only across handoff -> active. */
      handedBack: boolean;
      /** True while "Open live view again" fetches a fresh URL over an existing frame. */
      reopening: boolean;
    };

/** Header dock controls: stop confirmation, in-flight stop, or handoff/active actions. */
function BrowserActions({
  b,
  phase,
  fresh,
  wait,
  takingOver,
  onTakeover,
  onView,
  onAskStop,
  onKeep,
  onStop,
  t,
}: {
  b: BrowserTabState;
  phase: string;
  fresh: boolean;
  wait: boolean;
  takingOver: boolean;
  onTakeover: () => void;
  onView: () => void;
  onAskStop: () => void;
  onKeep: () => void;
  onStop: () => void;
  t: (english: string) => string;
}) {
  const live = phase === "active" || phase === "handoff";
  if (b.stop === "confirm") {
    return (
      <>
        <span className="bz-ask" role="alert">
          {t("Stop the session?")}
          {b.session?.signedIn ? ` ${t("Anything signed in to this session is lost.")}` : ""}
        </span>
        {/* One group: a long question never leaves either answer alone on a row. */}
        <span key="answers" className="bz-answers">
          <button key="go" type="button" className="btn btn-sm btn-danger" data-bz-act="go" onClick={onStop}>
            {t("Stop session")}
          </button>
          <button key="keep" type="button" className="btn btn-sm" data-bz-act="keep" onClick={onKeep}>
            {t("Keep it running")}
          </button>
        </span>
      </>
    );
  }
  if (b.stop === "stopping") {
    return (
      <button key="stopping" type="button" className="btn btn-sm btn-danger" disabled aria-disabled="true" aria-busy="true" aria-label={t("Stopping…")}>
        <span className="btn-spin" aria-hidden />
        <span className="bz-lbl">{t("Stopping…")}</span>
      </button>
    );
  }
  if (!live) return null;
  const spin = wait ? <span className="btn-spin" aria-hidden /> : null;
  return (
    <>
      {phase === "active" ? (
        <button
          key="takeover"
          type="button"
          className="btn btn-sm"
          data-bz-act="takeover"
          aria-disabled={takingOver || undefined}
          aria-busy={takingOver || undefined}
          aria-label={t("Take over")}
          title={t("Take over")}
          onClick={onTakeover}
        >
          {takingOver ? (
            <span className="btn-spin" aria-hidden />
          ) : (
            <Icon>
              <path d="M18 11V6a2 2 0 0 0-4 0v4M14 10V4a2 2 0 0 0-4 0v6M10 10.5V5a2 2 0 0 0-4 0v9" />
              <path d="M18 11a2 2 0 1 1 4 0v3a8 8 0 0 1-8 8h-2c-2.5 0-4.2-1-5.8-2.9L3.5 16a2 2 0 0 1 2.9-2.7L6 13.7" />
            </Icon>
          )}
          <span className="bz-lbl">{t("Take over")}</span>
        </button>
      ) : null}
      {phase === "handoff" && fresh ? (
        <button
          key="view"
          type="button"
          className="cv-iconbtn"
          data-bz-act="view"
          aria-disabled={wait || undefined}
          aria-busy={wait || undefined}
          aria-label={t("Open live view again")}
          title={t("Open live view again")}
          onClick={onView}
        >
          {spin ?? <Icon d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5" />}
        </button>
      ) : phase === "handoff" ? (
        <button
          key="view"
          type="button"
          className="btn btn-sm btn-primary"
          data-bz-act="view"
          aria-disabled={wait || undefined}
          aria-busy={wait || undefined}
          aria-label={t("Open live view")}
          title={t("Open live view")}
          onClick={onView}
        >
          {spin ?? <Icon d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" />}
          <span className="bz-lbl">{t("Open live view")}</span>
        </button>
      ) : null}
      {/* Distinct keys: React must never reuse the Open live view node as Stop
          session when a takeover ends under the person's focus. */}
      <button
        key="stop"
        type="button"
        className="btn btn-sm btn-danger"
        data-bz-act="stop"
        aria-label={t("Stop session")}
        title={t("Stop session")}
        onClick={b.stop === "failed" ? onStop : onAskStop}
      >
        <Icon><rect x="6" y="6" width="12" height="12" rx="1.5" /></Icon>
        <span className="bz-lbl">{t("Stop session")}</span>
      </button>
    </>
  );
}

/** The origin of a URL, or null when it does not parse. */
function originOf(value: string): string | null {
  try {
    return new URL(value).origin;
  } catch {
    return null;
  }
}

/**
 * The Browser tab's panel. Rendered while the tab exists and merely hidden
 * when another tab is active, so a live view in progress survives a tab
 * switch. The viewer URL lives only in this component's state, from the
 * person's click until the frame is torn down, and every teardown resets the
 * viewer to idle so the next open fetches a fresh URL. Failures the person
 * has to act on are toasts (`alertBrowser`); the panel itself stays plain.
 */
export default function BrowserPanel({ conversation, active, sheet, expanded, canExpand, onExpand, onClose }: {
  conversation: string;
  active: boolean;
  sheet: boolean;
  expanded: boolean;
  canExpand: boolean;
  onExpand: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const b = useBrowserTab();
  const [viewer, setViewer] = useState<ViewerState>({ phase: "idle" });
  const [startUrl, setStartUrl] = useState("");
  const [preview, setPreview] = useState<BrowserPreview | null>(null);
  const attempt = useRef(0);
  const frames = useRef(0);
  const dockRef = useRef<HTMLDivElement | null>(null);
  const root = useRef<HTMLDivElement | null>(null);
  const heading = useRef<HTMLElement | null>(null);
  // The control inside the panel that last held focus. Cleared the moment
  // focus leaves the panel on purpose; used only if that very node is removed.
  const claim = useRef<HTMLElement | null>(null);
  const st = browserStatus(b, t);
  const phase = b.session?.state ?? "none";
  const live = phase === "active" || phase === "handoff";
  const stopping = b.stop === "stopping";
  const viewing = viewer.phase === "loading" || viewer.phase === "open";
  // A handback tears a frame that is still loading down, and so does any
  // other non-handoff phase. A stop in flight does not: the view stays (inert)
  // until the stop has succeeded. A frame already open when the person presses
  // Done stays view-only (inert) while the assistant reads on: the same node,
  // never reloaded — even if a "Open live view again" reopen was in flight.
  const viewOnly = phase === "active" && viewer.phase === "open";
  // A new takeover never inherits the frame kept view-only: the person sees the
  // assistant's new request and opens a fresh view on purpose. It is hidden in
  // the same render the takeover arrives in (no moment where it takes input),
  // then torn down.
  const carried = phase === "handoff" && viewing && viewer.handedBack;
  const fresh = viewing && !carried;
  const showFrame = viewing && ((phase === "handoff" && !carried) || viewOnly);
  const asking = phase === "handoff" && !stopping && !showFrame;
  const wait = viewer.phase === "connecting" || (viewing && viewer.reopening && phase === "handoff");
  const starting = b.action === "starting";
  const takingOver = b.action === "taking_over";
  const frameBusy = wait || starting || takingOver || (showFrame && viewer.phase === "loading");
  const loadingUrl = showFrame && viewer.phase === "loading" ? viewer.url : null;
  const instructions = b.session?.instructions ?? "";
  const quota = b.session?.quota ?? null;

  // Where the header puts the address: inline while its whole host fits, else
  // on its own row. Measured again when the header or any item in it resizes,
  // on the next frame so the move never feeds back into the same observation.
  const head = useRef<HTMLElement | null>(null);
  const [addrBelow, setAddrBelow] = useState(false);
  const addrUrl = live && !showFrame ? b.session?.url ?? "" : "";
  useLayoutEffect(() => {
    const el = head.current;
    if (!el || !addrUrl) {
      setAddrBelow(false);
      return;
    }
    const measure = () => setAddrBelow(!hostFitsInRow(el));
    measure();
    if (typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    });
    ro.observe(el);
    for (const item of Array.from(el.children)) ro.observe(item);
    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
    };
  }, [addrUrl, b.stop, phase, viewing, wait, sheet, expanded, st.text]);

  useEffect(() => {
    if (viewOnly) {
      if (!viewer.handedBack || viewer.reopening) {
        if (viewer.reopening) attempt.current++;
        setViewer((v) => (v.phase === "open" ? { ...v, handedBack: true, reopening: false } : v));
      }
      return;
    }
    if (phase !== "handoff" || carried) {
      if (viewer.phase !== "idle") {
        attempt.current++;
        setViewer({ phase: "idle" });
      }
    }
  }, [phase, viewOnly, carried, viewer]);

  // Clicking directly into a cross-origin iframe from outside the panel moves
  // `document.activeElement` to the iframe without firing `focusin` on `#bz-panel`.
  useEffect(() => {
    if (!active || !showFrame) return;
    const onWindowBlur = () => {
      const at = document.activeElement;
      if (at instanceof HTMLIFrameElement && box.current?.contains(at)) {
        claim.current = at;
      }
    };
    window.addEventListener("blur", onWindowBlur);
    return () => window.removeEventListener("blur", onWindowBlur);
  }, [active, showFrame]);

  // Focus inside the visible frame goes with the takeover: it moves to the
  // tab, or the heading, never onto Stop, and only if the person is still
  // where the view was. An inactive tab keeps its hands off focus.
  const box = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!active || !viewOnly) return;
    const at = document.activeElement;
    if (at !== document.body && !(at && box.current?.contains(at))) return;
    claim.current = null;
    (document.getElementById("bz-tab") ?? heading.current)?.focus();
  }, [active, viewOnly]);

  // The frame is refused by the board's own CSP: say so now. Only an enforced
  // `frame-src` violation for the viewer's own origin counts. (A frame refused
  // by the provider's X-Frame-Options or frame-ancestors, or one that answers
  // with an error page, still fires `load`: nothing here can tell.)
  useEffect(() => {
    if (loadingUrl === null) return;
    const viewerOrigin = originOf(loadingUrl);
    const giveUp = () => {
      setViewer({ phase: "idle" });
      alertBrowser("Couldn't load the live view. Check your connection, or it may not be allowed on this board yet.");
    };
    const onViolation = (e: Event) => {
      const ev = e as Event & { effectiveDirective?: string; disposition?: string; blockedURI?: string };
      if (ev.disposition === "report" || ev.effectiveDirective !== "frame-src") return;
      if (viewerOrigin === null || originOf(ev.blockedURI ?? "") !== viewerOrigin) return;
      giveUp();
    };
    document.addEventListener("securitypolicyviolation", onViolation);
    // A frame that never answers is reported, not left spinning.
    const id = setTimeout(giveUp, FRAME_WAIT_MS);
    return () => {
      document.removeEventListener("securitypolicyviolation", onViolation);
      clearTimeout(id);
    };
  }, [loadingUrl]);

  // The control that held focus was removed (Stop, a refused frame): focus
  // must not fall to the page. It goes to the retry on a failure, and
  // otherwise to the tab, or to the panel's heading when the strip has no tab.
  // Once, and never when the person has moved on: a focus that left the panel
  // cleared the claim, and a focus that is still somewhere is left alone.
  useEffect(() => {
    const was = claim.current;
    if (!active || was === null || was.isConnected || document.activeElement !== document.body) return;
    claim.current = null;
    const specific =
      b.stop === "failed" ? '[data-bz-act="stop"]'
      : phase === "handoff" && !fresh ? '[data-bz-act="view"]'
      : null;
    (specific ? root.current?.querySelector<HTMLElement>(specific) : null)?.focus();
    if (document.activeElement === document.body) (document.getElementById("bz-tab") ?? heading.current)?.focus();
  });

  // A fast 503: the focus the stop left on the dock (or nowhere) goes to the retry.
  useEffect(() => {
    if (b.stop !== "failed") return;
    const at = document.activeElement;
    // Nowhere, the dock, or the places the rescue parks focus while a stop is
    // in flight (the heading or the tab): all of them are the panel's, not the person's choice.
    if (at === document.body || at === dockRef.current || at === heading.current || at === document.getElementById("bz-tab")) {
      root.current?.querySelector<HTMLElement>('[data-bz-act="stop"]')?.focus();
    }
  }, [b.stop]);

  const onFocusIn = (e: FocusEvent<HTMLElement>) => { claim.current = e.target; };
  const onFocusOut = (e: FocusEvent<HTMLElement>) => {
    const to = e.relatedTarget as Node | null;
    // To nothing (a click on bare page) or to anything outside the panel.
    if ((to === null && e.target.isConnected) || (to !== null && !e.currentTarget.contains(to))) claim.current = null;
  };

  // Focus a control in the panel, but only if the person's focus is still in
  // the panel (or nowhere): a late answer must never pull keystrokes away from
  // the chat draft, least of all into the provider's page.
  const focusSoon = (selector: string) => requestAnimationFrame(() => {
    const at = document.activeElement;
    if (at !== document.body && !root.current?.contains(at)) return;
    root.current?.querySelector<HTMLElement>(selector)?.focus();
  });

  const openView = async () => {
    const mine = ++attempt.current;
    const had = fresh;
    if (had) {
      setViewer((v) => (v.phase === "loading" || v.phase === "open" ? { ...v, reopening: true } : v));
    } else {
      setViewer({ phase: "connecting" });
    }
    try {
      const r = await viewBrowser(conversation);
      if (mine !== attempt.current) return;
      if (r.kind === "not_in_handoff") {
        // The takeover is over: the read that follows says so.
        setViewer({ phase: "idle" });
        void refreshBrowser();
        return;
      }
      setViewer({ phase: "loading", url: r.url, n: ++frames.current, handedBack: false, reopening: false });
      // Into the frame only if the person is still where they clicked (the
      // button, or nowhere) and nothing else has been opened since: a stop
      // confirm or the chat draft keeps the keystrokes.
      requestAnimationFrame(() => {
        const at = document.activeElement;
        if (at !== document.body && at?.getAttribute("data-bz-act") !== "view") return;
        if (browserSnapshot().stop !== "idle") return;
        root.current?.querySelector<HTMLElement>("[data-bz-frame]")?.focus();
      });
    } catch {
      if (mine !== attempt.current) return;
      // A failed reopen leaves the frame the person already has.
      if (had) {
        setViewer((v) => (v.phase === "loading" || v.phase === "open" ? { ...v, reopening: false } : v));
      } else {
        setViewer({ phase: "idle" });
      }
      alertBrowser("Couldn't open the live view.");
    }
  };

  const ask = () => {
    const confirming = needsStopConfirm(browserSnapshot());
    requestStop();
    if (confirming) focusSoon('[data-bz-act="keep"]');
  };
  const keep = () => {
    cancelStop();
    focusSoon('[data-bz-act="stop"]');
  };
  const stop = () => {
    void confirmStop();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Esc cancels a pending stop first; only then does it reach the drawer.
    if (e.key === "Escape" && b.stop === "confirm") {
      e.preventDefault();
      keep();
    }
  };

  const onView = () => { if (!wait) void openView(); };
  const onStart = async (e?: React.FormEvent) => {
    e?.preventDefault();
    if (starting) return;
    if (await startBrowserSession(startUrl)) void openView();
  };
  const onTakeover = async () => {
    if (takingOver) return;
    if (await takeoverBrowserSession()) void openView();
  };

  useEffect(() => {
    if (!active || phase !== "active" || showFrame) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    void previewBrowser(conversation).then((shot) => {
      if (!cancelled) setPreview(shot);
    }).catch(() => {
      if (!cancelled) setPreview(null);
    });
    return () => { cancelled = true; };
  }, [active, conversation, phase, showFrame, b.session?.url]);

  const hasDock = b.stop === "confirm" || stopping || live;
  const showPreview = phase === "active" && !showFrame && preview !== null;

  return (
    <div
      className="bz-panel"
      role="tabpanel"
      id="bz-panel"
      aria-labelledby="bz-heading"
      hidden={!active}
      ref={root}
      onKeyDown={onKeyDown}
      onFocus={onFocusIn}
      onBlur={onFocusOut}
    >
      <header ref={head} className="cv-head bz-head" data-confirm={b.stop === "confirm" || undefined} data-addr={addrBelow ? "below" : undefined}>
        {sheet ? (
          <button type="button" className="cv-iconbtn cv-back" onClick={onClose} aria-label={t("Back to chat")}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        ) : null}
        <span className="bz-globe"><GlobeIcon size={14} /></span>
        <span className="bz-title">
          <b id="bz-heading" tabIndex={-1} ref={heading}>{t("Browser")}</b>
          <span className="bz-badge" style={{ "--c": TONE[st.tone] } as React.CSSProperties} role="status">
            {phase === "active" && b.session?.signedIn ? (
              <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                <rect x="4" y="11" width="16" height="10" rx="2" />
                <path d="M8 11V7a4 4 0 0 1 8 0v4" />
              </svg>
            ) : null}
            {st.text}
          </span>
        </span>
        {live && !showFrame && b.session?.url ? <Address url={b.session.url} /> : <span className="bz-gap" />}
        {hasDock ? (
          <div className="bz-actions" ref={dockRef} data-bz-dock tabIndex={-1} role="group" aria-label={t("Browser session actions")}>
            <BrowserActions
              b={b}
              phase={phase}
              fresh={fresh}
              wait={wait}
              takingOver={takingOver}
              onTakeover={() => void onTakeover()}
              onView={onView}
              onAskStop={ask}
              onKeep={keep}
              onStop={stop}
              t={t}
            />
          </div>
        ) : null}
        {!sheet ? (
          <>
            <button type="button" className="cv-iconbtn" onClick={onExpand} disabled={!canExpand} aria-pressed={expanded} aria-label={t("Expand canvas")} title={t("Expand canvas")}>
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                {expanded ? <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /> : <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />}
              </svg>
            </button>
            <button type="button" className="cv-iconbtn" onClick={onClose} aria-label={t("Close canvas")}>
              <IconClose size={12} />
            </button>
          </>
        ) : null}
        {frameBusy ? (
          <div className="cv-bar" aria-hidden data-bz-loading>
            <i className="cv-bar-run" />
          </div>
        ) : null}
      </header>
      {/* Fixed slots, whatever the rest shows (stopping, a failed stop, a failed
          read): React must never remount the frame, which would load the bearer
          URL again. The frame is inert while the session is being stopped:
          nothing typed into a view that is going away reaches the page. */}
      <div className="cv-body bz-body" data-canvas-body data-frame={(showFrame || showPreview) || undefined}>
        {!live ? (
          <div className="bz-empty">
            <EmptyState title={b.action === "quota_limit" ? t("Workspace browser limit reached") : phase === "ended" ? t("Session ended") : t("No browser session")}>
              {b.action === "quota_limit" ? t("Stop an active session or wait for idle close.") : undefined}
            </EmptyState>
            <form className="bz-launch" onSubmit={(e) => void onStart(e)}>
              <input
                type="url"
                className="bz-url-input"
                value={startUrl}
                onChange={(e) => setStartUrl(e.target.value)}
                placeholder="https://…"
                aria-label={t("Starting URL")}
                disabled={starting}
              />
              <button
                type="submit"
                className="btn btn-sm btn-primary"
                data-bz-act="start"
                aria-disabled={starting || undefined}
                aria-busy={starting || undefined}
              >
                {starting ? <span className="btn-spin" aria-hidden /> : null}
                <span>{t("Start browser")}</span>
              </button>
            </form>
            {quota ? (
              <p className="bz-quota">
                {`${quota.active} / ${quota.limit} ${t("active")}`}
              </p>
            ) : null}
          </div>
        ) : null}
        {asking && instructions ? (
          <div className="bz-says">
            <small>{t("Your assistant:")}</small>
            <p id="bz-says-t">{instructions}</p>
            <p className="bz-says-note">{t("Read-only for your assistant once you finish.")}</p>
          </div>
        ) : null}
        {showPreview ? (
          <div className="bz-preview">
            <img src={`data:image/jpeg;base64,${preview.data}`} alt={t("Current browser page")} />
          </div>
        ) : null}
        {showFrame && (viewer.phase === "loading" || viewer.phase === "open") ? (
          <div className="bz-viewer" ref={box} data-phase={viewer.phase} inert={stopping || viewOnly || undefined}>
            <iframe
              key={viewer.n}
              data-bz-frame
              title={t("Live view")}
              src={viewer.url}
              // The provider's page needs its own scripts and forms; it is a
              // different origin (the client refuses the board's own), so none
              // of this reaches the board.
              sandbox="allow-scripts allow-same-origin allow-forms"
              referrerPolicy="no-referrer"
              onLoad={() => setViewer((v) => (v.phase === "loading" ? { ...v, phase: "open" } : v))}
            />
          </div>
        ) : null}
        {/* Outside the inert box, which assistive tech skips; always mounted so live announcements fire reliably. */}
        <span className="sr-only" role="status">{viewOnly ? t("Live view, view only while the assistant reads.") : ""}</span>
      </div>
    </div>
  );
}

/** The thread's card for the session: a way back to a hidden tab, and to the
 *  panel when nothing else opened the drawer. Absent until there is a session
 *  to speak of. */
export function BrowserCard() {
  const t = useTranslate();
  const b = useBrowserTab();
  const canvas = useCanvas();
  if (b.availability !== "available") return null;
  const phase = b.session?.state ?? "none";
  // An ended session has nothing to open, and the card sits under the live turn.
  if (phase === "none" || phase === "ended") return null;
  const shown = isBrowserPanelShown(canvas, b);
  const statusText = browserStatus(b, t).text;
  const handoff = phase === "handoff" && !b.loadError && b.stop !== "stopping";
  const openLabel = t("Open Browser");
  // Only the real host, never the raw address (a look-alike can hide behind
  // userinfo), on a line of its own and never preceded by words that could push
  // its end out of the box: when it is too long, the START is what is clipped.
  const host = (phase === "active" || phase === "handoff") && b.session?.url ? (addressParts(b.session.url)?.host ?? "") : "";
  return (
    <div className="bz-card-wrap">
      {/* CAD-2440: the card system's summary shape, same markup and hooks. */}
      <TurnCard
        shape="button"
        kind="browser"
        className="bz-card"
        data-kind="session"
        open={shown}
        aria-label={[openLabel, statusText, host].filter(Boolean).join(" · ")}
        onClick={openBrowser}
        icon={<GlobeIcon size={16} />}
        subject={t("Browser")}
        meta={
          <>
            <span className="artmeta">{statusText}</span>
            {host ? <span className="artmeta bz-card-host" title={host}><span>{host}</span></span> : null}
          </>
        }
        trailing={handoff ? <span className="artopen">{openLabel}</span> : undefined}
      />
    </div>
  );
}

/** Header toggle for the Agent Browser drawer: gated on availability,
 *  carries the live status dot, and opens or closes the Browser tab. */
export function BrowserHeaderToggle() {
  const t = useTranslate();
  const b = useBrowserTab();
  const canvas = useCanvas();
  if (b.availability !== "available") return null;
  const shown = isBrowserPanelShown(canvas, b);
  const st = browserStatus(b, t);
  const label = `${t(shown ? "Hide Browser" : "Open Browser")} · ${st.text}`;
  return (
    <button
      type="button"
      className="btn btn-ghost bz-header-toggle"
      data-browser-toggle=""
      aria-pressed={shown}
      aria-controls="bz-panel"
      aria-label={label}
      title={label}
      onClick={() => (shown ? closeBrowserDrawer() : openBrowser())}
    >
      <GlobeIcon size={14} />
      <span className="bz-header-toggle-label">{t("Browser")}</span>
      <span
        className="bz-dot"
        style={{ "--c": TONE[st.tone] } as React.CSSProperties}
        data-pulse={st.pulse || undefined}
        aria-hidden
      />
    </button>
  );
}
