import { useCallback, useEffect, useRef, useState } from "react";
import type { Route } from "../../lib/router";
import { fmtBytes } from "../../lib/fmt";
import { useLocale, useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";
import { IconLock, IconRestore, IconWarning } from "../../ui/icons";
import { UNVERIFIED_WRITE_BLOCK } from "../projects/drawerArchiveGate";
import { wiki, WikiError, type TrashEntry, type TrashLanding } from "./api";
import { useWikiArchiveBlock } from "./archiveGate";
import { baseName } from "./paths";
import { confirmedRestoreDestination, destinationError, kindWord, mergeEntries, mergeLandings, restoreBlock } from "./trash";
import { EmptyCard, Failure, KindIcon, Loading, Note } from "./shared";

/**
 * The wiki's Trash (CAD-2018), the operator's one screen for deleted
 * landings: the list, one landing's original paths, and a single-landing
 * restore through the existing `POST /api/wiki/mv`.
 *
 * Everything shown is the board's answer. The screen never invents a
 * `.trash/…` selector (a landing may have no provable origin), never asks
 * for a landing's items without one, never predicts a destination
 * collision (the board answers `409 path_conflict`), and never treats a
 * digest checkpoint as custody. A board without the listing route reports
 * it unavailable instead of breaking wiki browsing.
 */

type Tone = "ok" | "warn" | "fail";

interface Outcome {
  tone: Tone;
  title: string;
  body: string;
  /** The board's own refusal text, shown verbatim and never reworded. */
  detail?: string;
  /** A stable board code (e.g. `custody_unverified`), shown as data. */
  code?: string | null;
  retry?: boolean;
  reload?: boolean;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function aborted(error: unknown): boolean {
  if (typeof error !== "object" || error === null || !("name" in error)) return false;
  return error.name === "AbortError";
}

export interface TrashPaneProps {
  /** The proven operator, a proven non-operator, or unknown (never guessed). */
  operator?: boolean | null;
  navHref: (route: Route) => string;
  readOnly: boolean;
  readOnlyReason?: string | null;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
  /** Reload the tree after a restore changed what exists. */
  onRefresh: () => void;
}

export default function TrashPane(props: TrashPaneProps) {
  // A proven non-operator gets no discovery and no request: the server
  // refuses both trash routes anyway, and asking would only leak which
  // route a board serves.
  if (props.operator === false) return <TrashDenied navHref={props.navHref} />;
  return <TrashBrowser {...props} />;
}

function TrashDenied({ navHref }: { navHref: (route: Route) => string }) {
  const t = useTranslate();
  return (
    <>
      <TrashBar navHref={navHref} leaf={t("Trash")} />
      <PageState
        title={t("Trash is operator-only")}
        icon={<IconLock size={32} />}
        actions={<Button href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>{t("Back to Wiki")}</Button>}
      >
        {t("Restoring from .trash/ is the workspace owner’s. Ask an operator to inspect a deletion and restore it when its stored files are available.")}
      </PageState>
    </>
  );
}

function TrashBar({ navHref, leaf }: {
  navHref: (route: Route) => string;
  leaf: string;
}) {
  const t = useTranslate();
  return (
    <div className="wk-bar">
      <div className="wk-crumbs">
        <Link href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>{t("Wiki")}</Link>
        <span className="wk-crumb">
          <span className="wk-sep">/</span>
          <b>{leaf}</b>
        </span>
      </div>
      <div className="wk-tools">
        <Button href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>{t("Back to Wiki")}</Button>
      </div>
    </div>
  );
}

function TrashBrowser({ navHref, readOnly, readOnlyReason, onToast, onRefresh }: TrashPaneProps) {
  const t = useTranslate();
  const [landings, setLandings] = useState<TrashLanding[] | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "denied" | "unavailable" | "error">("loading");
  const [error, setError] = useState("");
  const [more, setMore] = useState(false);
  const [moreError, setMoreError] = useState<string | null>(null);
  const [selected, setSelected] = useState<TrashLanding | null>(null);
  const [announce, setAnnounce] = useState("");
  const rootRef = useRef<HTMLDivElement>(null);
  const listHeading = useRef<HTMLHeadingElement>(null);
  const returnTo = useRef<string | null>(null);
  const generation = useRef(0);

  const load = useCallback(async (after: string | null, append: boolean) => {
    const run = ++generation.current;
    if (!append) setStatus("loading");
    try {
      const page = await wiki.trash(after);
      if (run !== generation.current) return;
      if (page.next_cursor && page.next_cursor === after) {
        throw new Error("The board repeated a trash cursor; more landings may be missing.");
      }
      setLandings((current) => (append && current ? mergeLandings(current, page.landings) : page.landings));
      setCursor(page.next_cursor);
      setStatus("ready");
      setMoreError(null);
      setAnnounce(t("Trash landings loaded."));
    } catch (caught) {
      if (run !== generation.current || aborted(caught)) return;
      if (append) {
        setMoreError(message(caught));
        return;
      }
      setLandings(null);
      if (caught instanceof WikiError && caught.status === 403) {
        setStatus("denied");
        setError(caught.message);
      } else if (
        caught instanceof WikiError &&
        (caught.status === 404 || (caught.status === 501 && caught.code === "not_implemented"))
      ) {
        // Older native boards answer 404; older hosted boards use a typed
        // 501 catch-all for operations the isolated board does not support.
        setStatus("unavailable");
      } else {
        setStatus("error");
        setError(message(caught));
      }
    }
  }, [t]);

  useEffect(() => {
    void load(null, false);
  }, [load]);

  // Returning from a landing puts focus back on that landing's row, or on
  // the list heading when the landing was just restored away.
  useEffect(() => {
    if (selected !== null) return;
    const landingId = returnTo.current;
    if (!landingId) return;
    returnTo.current = null;
    const openers = rootRef.current?.querySelectorAll<HTMLButtonElement>("button[data-landing]");
    const row = openers ? Array.from(openers).find((button) => button.dataset.landing === landingId) : undefined;
    (row ?? listHeading.current)?.focus();
  }, [selected]);

  const openLanding = (landing: TrashLanding) => {
    setSelected(landing);
  };

  const backToList = () => {
    returnTo.current = selected?.landing_id ?? null;
    setSelected(null);
  };

  const reload = () => {
    setLandings(null);
    setCursor(null);
    void load(null, false);
  };

  const restored = (to: string) => {
    setLandings((current) => current?.filter((landing) => landing.landing_id !== selected?.landing_id) ?? current);
    onRefresh();
    onToast("ok", `restored ${baseName(to)}`);
  };

  return (
    <div ref={rootRef}>
      <TrashBar
        navHref={navHref}
        leaf={selected ? (selected.path ? baseName(selected.path) : t("Landing")) : t("Trash")}
      />
      {selected ? (
        <LandingDetail
          key={selected.landing_id}
          landing={selected}
          navHref={navHref}
          readOnly={readOnly}
          readOnlyReason={readOnlyReason}
          onBack={backToList}
          onRestored={restored}
          onReload={() => { backToList(); reload(); }}
        />
      ) : (
        <section aria-labelledby="wk-trash-title">
          <div className="wk-pagehead">
            <h1 className="wk-title" id="wk-trash-title" tabIndex={-1} ref={listHeading}>{t("Trash")}</h1>
            <span className="wk-meta">{t("Operator-only · landings under .trash/")}</span>
          </div>

          {status === "ready" && <Note>
            {t("Deleting moves a page or folder into one landing, which references any stored attachments. Restoring moves the landing back; the board checks attachment storage before the move.")}
          </Note>}

          <p className="sr-only" role="status">{announce}</p>

          {status === "loading" && <Loading what="reading the trash…" />}

          {status === "denied" && (
            <PageState
              title={t("Trash is operator-only")}
              icon={<IconLock size={32} />}
              actions={<Button href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>{t("Back to Wiki")}</Button>}
            >
              {t("The board refused this listing before any write.")}
              {error && <span className="num"> {error}</span>}
            </PageState>
          )}

          {status === "unavailable" && (
            <PageState
              title={t("Trash browsing isn’t available on this board")}
              icon={<IconWarning size={32} />}
              actions={<Button href={navHref({ screen: "wiki", mode: "browse", path: null, query: null })}>{t("Back to Wiki")}</Button>}
            >
              {t("This board does not serve the trash listing route, so this screen cannot list deletions or offer a restore. Keep any delete receipt and ask an operator about recovery.")}
            </PageState>
          )}

          {status === "error" && (
            <Failure what="could not read the trash" error={error} onRetry={reload} />
          )}

          {status === "ready" && landings !== null && landings.length === 0 && !cursor && (
            <EmptyCard title={t("Trash is empty")}>
              {t("Nothing has been deleted, or an operator already restored everything.")}
            </EmptyCard>
          )}

          {status === "ready" && landings !== null && (
            <>
              {landings.length > 0 && <TrashTable landings={landings} onOpen={openLanding} />}
              {moreError && (
                <Note warn>
                  {t("More landings could not be read.")} <span className="num">{moreError}</span>
                </Note>
              )}
              {cursor && (
                <div className="wk-trash-more">
                  <Button
                    loading={more}
                    onClick={() => {
                      setMore(true);
                      void load(cursor, true).finally(() => setMore(false));
                    }}
                  >
                    {t("Load more landings")}
                  </Button>
                </div>
              )}
            </>
          )}
        </section>
      )}
    </div>
  );
}

function TrashTable({ landings, onOpen }: {
  landings: TrashLanding[];
  onOpen: (landing: TrashLanding) => void;
}) {
  const t = useTranslate();
  const { formatRelativeTime } = useLocale();
  return (
    <div
      className="wk-trash-tablewrap"
      role="region"
      aria-label={t("Deleted landings")}
      tabIndex={0}
    >
      <table className="wk-trash-table">
        <caption className="sr-only">{t("Deleted landings")}</caption>
        <thead>
          <tr>
            <th scope="col">{t("Original path")}</th>
            <th scope="col">{t("Deleted")}</th>
            <th scope="col">{t("By")}</th>
            <th scope="col" className="wk-num">{t("Items")}</th>
            <th scope="col" className="wk-num">{t("Stored")}</th>
            <th scope="col">{t("State")}</th>
          </tr>
        </thead>
        <tbody>
          {landings.map((landing) => {
            const blocked = restoreBlock(landing);
            const unknownOrigin = landing.from === null;
            return (
              <tr key={landing.landing_id}>
                <td>
                  <span className="wk-trash-path">
                    {landing.kind && <span className="wk-tkind"><KindIcon kind={landing.kind} /></span>}
                    <button
                      type="button"
                      className="wk-trash-open"
                      data-landing={landing.landing_id}
                      onClick={() => onOpen(landing)}
                    >
                      {landing.path ?? t("Original path unknown")}
                    </button>
                  </span>
                  <span className="wk-trash-id num">{landing.landing_id}</span>
                </td>
                <td data-label={t("Deleted")}>
                  {landing.deleted_at === null ? (
                    <span className="wk-trash-quiet">{t("unknown")}</span>
                  ) : (
                    <time dateTime={new Date(landing.deleted_at).toISOString()} title={new Date(landing.deleted_at).toLocaleString()}>
                      {formatRelativeTime(new Date(landing.deleted_at))}
                    </time>
                  )}
                </td>
                <td data-label={t("By")}>
                  {landing.actor ?? <span className="wk-trash-quiet">{t("unknown")}</span>}
                </td>
                <td data-label={t("Items")} className="wk-num">
                  {landing.item_count ?? <span className="wk-trash-quiet">{t("unknown")}</span>}
                </td>
                <td data-label={t("Stored")} className="wk-num">
                  {landing.size === null ? <span className="wk-trash-quiet">{t("unknown")}</span> : fmtBytes(landing.size)}
                </td>
                <td data-label={t("State")}>
                  {unknownOrigin ? (
                    <span className="wk-badge wk-badge-warn" title={blocked?.code ?? undefined}>{t("origin unknown")}</span>
                  ) : landing.restorable ? (
                    <span className="wk-badge wk-badge-ok">{t("restorable")}</span>
                  ) : (
                    <span className="wk-badge wk-badge-warn" title={blocked?.code ?? undefined}>{t("not restorable")}</span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function LandingDetail({ landing, navHref, readOnly, readOnlyReason, onBack, onRestored, onReload }: {
  landing: TrashLanding;
  navHref: (route: Route) => string;
  readOnly: boolean;
  readOnlyReason?: string | null;
  onBack: () => void;
  onRestored: (to: string) => void;
  onReload: () => void;
}) {
  const t = useTranslate();
  const { formatRelativeTime } = useLocale();
  const [entries, setEntries] = useState<TrashEntry[] | null>(null);
  const [itemsCursor, setItemsCursor] = useState<string | null>(null);
  const [itemsStatus, setItemsStatus] = useState<"skipped" | "loading" | "ready" | "error">(
    landing.from === null ? "skipped" : "loading",
  );
  const [itemsError, setItemsError] = useState("");
  const [itemsMore, setItemsMore] = useState(false);
  const [destination, setDestination] = useState(landing.path ?? "");
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [serverBlock, setServerBlock] = useState<{ text: string; code: string | null } | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [consumed, setConsumed] = useState<{ to: string; items: number | null } | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const successHeading = useRef<HTMLHeadingElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  // One landing is one move: a second click before the first answer must
  // not send a second restore.
  const inFlight = useRef(false);
  const sourceArchiveBlock = useWikiArchiveBlock(landing.path);
  const destinationArchiveBlock = useWikiArchiveBlock(destination.trim(), true);
  const archiveBlock = !landing.path
    ? UNVERIFIED_WRITE_BLOCK
    : sourceArchiveBlock ?? destinationArchiveBlock;

  const block = restoreBlock(landing);
  const blocked = block ?? serverBlock;
  const canEditDestination = !blocked && !readOnly && landing.from !== null && !outcome?.reload;
  const canRestore = canEditDestination && archiveBlock === null;

  useEffect(() => {
    heading.current?.focus();
  }, []);

  useEffect(() => {
    if (consumed) successHeading.current?.focus();
  }, [consumed]);

  // Only a landing with a provable restore source has addressable paths.
  useEffect(() => {
    const from = landing.from;
    if (from === null) return;
    let cancelled = false;
    setItemsStatus("loading");
    wiki
      .trashItems(from)
      .then((page) => {
        if (cancelled) return;
        setEntries(page.entries);
        setItemsCursor(page.next_cursor);
        setItemsStatus("ready");
      })
      .catch((caught) => {
        if (cancelled || aborted(caught)) return;
        setItemsStatus("error");
        setItemsError(message(caught));
      });
    return () => {
      cancelled = true;
    };
  }, [landing.from]);

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (confirming && !element.open) element.showModal();
    else if (!confirming && element.open) element.close();
  }, [confirming]);

  const loadMoreItems = async () => {
    if (!landing.from || !itemsCursor) return;
    setItemsMore(true);
    try {
      const page = await wiki.trashItems(landing.from, itemsCursor);
      if (page.next_cursor && page.next_cursor === itemsCursor) {
        throw new Error("The board repeated a trash cursor; more paths may be missing.");
      }
      setEntries((current) => mergeEntries(current ?? [], page.entries));
      setItemsCursor(page.next_cursor);
    } catch (caught) {
      if (!aborted(caught)) setItemsError(message(caught));
    } finally {
      setItemsMore(false);
    }
  };

  const localError = destinationError(destination);

  // The confirmation dialog opens only for a destination that passes the
  // local pre-flight; the board still makes the real decision.
  const requestRestore = () => {
    if (!canRestore || busy) return;
    if (localError) {
      setTouched(true);
      input.current?.focus();
      return;
    }
    setConfirming(true);
  };

  const attempt = async () => {
    const from = landing.from;
    if (from === null || inFlight.current || !canRestore) return;
    const to = destination.trim();
    const invalid = destinationError(to);
    if (invalid) {
      setTouched(true);
      input.current?.focus();
      return;
    }
    inFlight.current = true;
    setBusy(true);
    setOutcome(null);
    try {
      const result = await wiki.mv(from, to);
      const restoredTo = confirmedRestoreDestination(result, from, to);
      if (restoredTo === null) throw new Error("The board returned an incomplete restore receipt.");
      setConsumed({ to: restoredTo, items: landing.item_count });
      onRestored(restoredTo);
    } catch (caught) {
      const refusal = caught instanceof WikiError ? caught : null;
      if (refusal?.status === 403) {
        setOutcome({
          tone: "fail",
          title: "Restore is operator-only",
          body: "The board refused the restore before any write.",
          detail: refusal.message,
          code: refusal.code ?? null,
        });
      } else if (refusal?.code === "custody_unverified") {
        const next = {
          code: "custody_unverified",
          text: "The board cannot prove custody of this landing’s stored bytes, so it refused the restore. A digest checkpoint from an older board is not custody; this screen will not treat one as permission.",
        };
        setServerBlock(next);
        setOutcome({
          tone: "warn",
          title: "Restore blocked — custody not verified",
          body: next.text,
          detail: refusal.message,
          code: next.code,
        });
      } else if (refusal?.code === "verification_required") {
        const next = {
          code: "verification_required",
          text: "This board wants a digest checkpoint before a large restore. A checkpoint counts files once; it does not prove their bytes are still there, so this screen does not use one as permission to restore.",
        };
        setServerBlock(next);
        setOutcome({
          tone: "warn",
          title: "Restore blocked — a checkpoint is not custody",
          body: next.text,
          detail: refusal.message,
          code: next.code,
        });
      } else if (refusal?.status === 409) {
        setOutcome({
          tone: "warn",
          title: refusal.code === "path_conflict" ? "That destination already exists" : "The board refused the restore",
          body: refusal.code === "path_conflict"
            ? "Nothing was written. Choose another destination and restore again."
            : "The board refused this request. Reload Trash before retrying if the result is uncertain.",
          detail: refusal.message,
          code: refusal.code ?? null,
          ...(refusal.code === "path_conflict" ? {} : { reload: true }),
        });
      } else if (refusal?.status === 404) {
        setOutcome({
          tone: "fail",
          title: "The landing or stored bytes are unavailable",
          body: "The board refused the restore. Reload Trash to check whether the landing is still available.",
          detail: refusal.message,
          code: refusal.code ?? null,
          reload: true,
        });
      } else if (refusal?.status === 503 && refusal.code === "capability_unavailable") {
        setOutcome({
          tone: "warn",
          title: "The stored bytes could not be checked",
          body: "The board could not verify the landing, so nothing was restored. Retry when storage answers.",
          detail: refusal.message,
          code: refusal.code ?? null,
          retry: true,
        });
      } else if (refusal?.status === 400) {
        setOutcome({
          tone: "fail",
          title: "That destination was refused",
          body: "The board refused this request. Reload Trash before retrying if the result is uncertain.",
          detail: refusal.message,
          code: refusal.code ?? null,
          ...(refusal.code === "invalid_request" ? {} : { reload: true }),
        });
      } else {
        // A failed or malformed answer is not proof that the move did not happen.
        setOutcome({
          tone: "warn",
          title: "The restore was not confirmed",
          body: "The board did not confirm whether the landing moved. Reload Trash and check the list before trying again.",
          ...(refusal ? { detail: refusal.message, code: refusal.code ?? null } : { detail: message(caught) }),
          reload: true,
        });
      }
      if (refusal?.status === 409 || refusal?.status === 400) input.current?.focus();
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  if (consumed) {
    const count = consumed.items;
    return (
      <section aria-labelledby="wk-restored-title">
        <div className="wk-pagehead">
          <h1 className="wk-title" id="wk-restored-title" tabIndex={-1} ref={successHeading}>{t("Restored")}</h1>
          <span className="wk-meta num">{consumed.to}</span>
        </div>
        <Note>
          {count === null
            ? t("The landing moved back. The board did not report how many entries it held.")
            : `${count} ${t(count === 1 ? "entry" : "entries")} ${t("moved back from the landing.")}`}
        </Note>
        <Note warn>
          {t("The board checked the stored bytes before moving them. A stored object deleted between that check and the move cannot be prevented, so open the restored files to confirm them.")}
        </Note>
        <div className="wk-actions">
          <Button variant="primary" href={navHref({ screen: "wiki", mode: "browse", path: consumed.to, query: null })}>
            {t("Open the restored path")}
          </Button>
          <Button onClick={onBack}>{t("Back to Trash")}</Button>
        </div>
      </section>
    );
  }

  const itemsLabel = entries === null
    ? t("Reading the landing’s paths…")
    : `${entries.length} ${t(entries.length === 1 ? "path listed" : "paths listed")}`;

  return (
    <>
      <div className="wk-pagehead">
        <h1 className="wk-title" tabIndex={-1} ref={heading}>
          {landing.path ? baseName(landing.path) : t("Landing")}
        </h1>
        <span className="wk-meta">
          {landing.path
            ? <span className="num">{landing.path}</span>
            : t("The board cannot prove this landing’s original path.")}
        </span>
      </div>

      <div className="wk-trash-split">
        <section className="card wk-trash-panel" aria-labelledby="wk-contents-title">
          <h2 className="wk-h2" id="wk-contents-title">{t("Landing contents")}</h2>
          <p className="wk-sub">
            {landing.from === null
              ? t("This landing has no provable restore source, so its paths are not requested.")
              : itemsLabel}
          </p>

          {landing.from === null ? (
            <Note warn>{block?.text ? t(block.text) : t("The board cannot name this landing’s restore source.")}</Note>
          ) : itemsStatus === "loading" ? (
            <Loading what="reading the landing’s paths…" />
          ) : itemsStatus === "error" ? (
            <Failure what="could not read this landing’s paths" error={itemsError} />
          ) : entries !== null && entries.length === 0 ? (
            <p className="wk-sub">{t("The board listed no paths in this landing.")}</p>
          ) : (
            <>
              <ul className="wk-trash-items">
                {(entries ?? []).map((entry) => (
                  <li key={`${entry.kind}:${entry.path}`}>
                    <span className="wk-tkind"><KindIcon kind={entry.kind} /></span>
                    <span className="wk-trash-item-path num">{entry.path}</span>
                    <span className="wk-trash-item-size num">
                      {entry.size === null ? t("unknown") : fmtBytes(entry.size)}
                    </span>
                  </li>
                ))}
              </ul>
              {itemsError && <Note warn>{itemsError}</Note>}
              {itemsCursor && (
                <div className="wk-trash-more">
                  <Button loading={itemsMore} onClick={() => void loadMoreItems()}>{t("Load more paths")}</Button>
                </div>
              )}
            </>
          )}
        </section>

        <section className="card wk-trash-panel" aria-labelledby="wk-restore-title">
          <h2 className="wk-h2" id="wk-restore-title">{t("Restore")}</h2>

          <dl className="wk-trash-kv">
            <div><dt>{t("Landing")}</dt><dd className="num">{landing.landing_id}</dd></div>
            <div>
              <dt>{t("Restore source")}</dt>
              <dd className="num">{landing.from ?? t("unknown")}</dd>
            </div>
            <div>
              <dt>{t("Original path")}</dt>
              <dd className="num">{landing.path ?? t("unknown")}</dd>
            </div>
            <div>
              <dt>{t("Deleted")}</dt>
              <dd>
                {landing.deleted_at === null ? t("unknown") : formatRelativeTime(new Date(landing.deleted_at))}
                {landing.actor ? ` · ${landing.actor}` : ""}
              </dd>
            </div>
            <div>
              <dt>{t("Items")}</dt>
              <dd>{landing.item_count === null ? t("unknown") : landing.item_count}</dd>
            </div>
            <div>
              <dt>{t("Stored")}</dt>
              <dd>{landing.size === null ? t("unknown") : fmtBytes(landing.size)}</dd>
            </div>
          </dl>

          {blocked && (
            <Note warn>
              <IconWarning size={13} />
              <span>
                {t(blocked.text)}
                {blocked.code && <> <span className="num wk-trash-code">{blocked.code}</span></>}
              </span>
            </Note>
          )}

          {readOnly && (
            <Note warn>{readOnlyReason ?? t("Writes are disabled on this board.")}</Note>
          )}
          {archiveBlock && <Note warn>{archiveBlock}</Note>}

          {landing.from !== null && (
            <>
              <label className="wk-trash-label" htmlFor="wk-restore-dest">{t("Restores to")}</label>
              <input
                id="wk-restore-dest"
                ref={input}
                className="field wk-trash-dest"
                type="text"
                value={destination}
                spellCheck={false}
                autoComplete="off"
                autoCapitalize="off"
                disabled={!canEditDestination || busy}
                aria-invalid={touched && localError ? "true" : "false"}
                aria-describedby="wk-restore-hint wk-restore-state"
                onChange={(event) => { setTouched(true); setDestination(event.target.value); }}
                onBlur={() => setTouched(true)}
                onKeyDown={(event) => {
                  if (event.key !== "Enter") return;
                  event.preventDefault();
                  requestRestore();
                }}
              />
              <p className="wk-trash-hint" id="wk-restore-hint">
                {t("The board checks the destination and every grant before the first write. Nothing is copied.")}
              </p>
              <p className={`wk-trash-state${touched && localError ? " fail" : ""}`} id="wk-restore-state">
                {touched && localError ? t(localError) : ""}
              </p>
              <div className="wk-actions">
                <Button
                  variant="primary"
                  icon={<IconRestore />}
                  disabled={!canRestore}
                  loading={busy}
                  onClick={requestRestore}
                >
                  {`${t("Restore")} ${t(kindWord(landing.kind))}`}
                </Button>
                <Button onClick={onBack}>{t("Back to Trash")}</Button>
              </div>
            </>
          )}

          {landing.from === null && (
            <div className="wk-actions">
              <Button onClick={onBack}>{t("Back to Trash")}</Button>
            </div>
          )}

          {outcome && (
            <div className={`wk-result ${outcome.tone}`} role={outcome.tone === "fail" ? "alert" : "status"}>
              <div className="wk-result-title">
                <span>{t(outcome.title)}</span>
                {outcome.code && <span className="num wk-result-code">{outcome.code}</span>}
              </div>
              <p className="wk-result-body">{t(outcome.body)}</p>
              {outcome.detail && <p className="wk-result-detail num">{outcome.detail}</p>}
              {(outcome.retry || outcome.reload) && (
                <div className="wk-actions">
                  {outcome.retry && <Button onClick={() => void attempt()}>{t("Retry restore")}</Button>}
                  {outcome.reload && <Button onClick={onReload}>{t("Reload Trash")}</Button>}
                </div>
              )}
            </div>
          )}
        </section>
      </div>

      {landing.from !== null && (
        <dialog
          ref={dialog}
          className="wk-trash-dialog"
          aria-labelledby="wk-restore-confirm-title"
          aria-describedby="wk-restore-confirm-summary"
          onCancel={(event) => { event.preventDefault(); setConfirming(false); }}
          onClose={() => setConfirming(false)}
        >
          <h2 id="wk-restore-confirm-title" className="wk-dialog-title">{t("Restore from Trash?")}</h2>
          <p id="wk-restore-confirm-summary" className="wk-trash-hint">
            {landing.item_count === null
              ? t("The landing’s entries move back to the path below. Nothing is copied, and the landing is consumed.")
              : `${landing.item_count} ${t(landing.item_count === 1 ? "entry" : "entries")} ${t("move back to the path below. Nothing is copied, and the landing is consumed.")}`}
          </p>
          <dl className="wk-trash-kv">
            <div><dt>{t("Landing")}</dt><dd className="num">{landing.from}</dd></div>
            <div><dt>{t("Restores to")}</dt><dd className="num">{destination.trim()}</dd></div>
          </dl>
          <div className="wk-actions">
            <Button onClick={() => setConfirming(false)}>{t("Cancel")}</Button>
            <Button variant="primary" loading={busy} disabled={!canRestore} onClick={() => { setConfirming(false); void attempt(); }}>
              {t("Restore")}
            </Button>
          </div>
        </dialog>
      )}
    </>
  );
}
