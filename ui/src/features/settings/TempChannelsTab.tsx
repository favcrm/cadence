import { useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import type {
  TempChannelAuditCounts,
  TempChannelAuditDetailPayload,
  TempChannelAuditItem,
  TempChannelAuditStatusFilter,
  TempChannelDecision,
  TempChannelMessage,
} from "../../lib/types";
import Button from "../../ui/Button";
import PageState from "../../ui/PageState";
import { IconRefresh, IconWarning } from "../../ui/icons";
import SettingsDrawer from "./SettingsDrawer";
import {
  formatChannelExpiry,
  shortTempChannelId,
  tempChannelCountsSummary,
  tempChannelStatusView,
  triageStatusView,
} from "./tempChannelsView";

const PAGE_SIZE = 20;

function chipToneClass(tone: "ok" | "warn" | "fail" | "muted"): string {
  switch (tone) {
    case "ok":
      return "custmcp-chip-ok";
    case "warn":
      return "custmcp-chip-warn";
    case "fail":
      return "custmcp-chip-fail";
    case "muted":
      return "bg-ink-800 text-ink-400";
  }
}

export default function TempChannelsTab({
  canWrite,
  initialOpenId = null,
  onCountsChange,
}: {
  canWrite: boolean;
  initialOpenId?: string | null;
  onCountsChange?: (counts: TempChannelAuditCounts) => void;
}) {
  const t = useTranslate();
  const [filter, setFilter] = useState<TempChannelAuditStatusFilter>("all");
  const [searchInput, setSearchInput] = useState("");
  const [searchApplied, setSearchApplied] = useState("");
  const [rows, setRows] = useState<TempChannelAuditItem[]>([]);
  const [total, setTotal] = useState(0);
  const [counts, setCounts] = useState<TempChannelAuditCounts>({
    all: 0,
    open: 0,
    needs_human: 0,
    ended: 0,
  });
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [openId, setOpenId] = useState<string | null>(initialOpenId);
  const [reloadTick, setReloadTick] = useState(0);
  const onCountsRef = useRef(onCountsChange);
  onCountsRef.current = onCountsChange;

  useEffect(() => {
    const timer = setTimeout(() => {
      setSearchApplied(searchInput.trim());
    }, 150);
    return () => clearTimeout(timer);
  }, [searchInput]);

  useEffect(() => {
    const controller = new AbortController();
    let active = true;
    setLoading(true);
    setError(null);
    setUnavailable(false);

    void api
      .tempChannelsList(
        {
          status: filter,
          ...(searchApplied ? { q: searchApplied } : {}),
          limit: PAGE_SIZE,
          offset: 0,
        },
        controller.signal,
      )
      .then((res) => {
        if (!active) return;
        setRows(res.channels ?? []);
        setTotal(res.total ?? 0);
        if (res.counts) {
          setCounts(res.counts);
          onCountsRef.current?.(res.counts);
        }
      })
      .catch((e: unknown) => {
        if (!active || (e instanceof DOMException && e.name === "AbortError")) return;
        if (e instanceof ApiError && (e.status === 404 || e.status === 501)) {
          setUnavailable(true);
          return;
        }
        setError(e instanceof Error ? e.message : t("Could not load temporary channels."));
      })
      .finally(() => {
        if (active) setLoading(false);
      });

    return () => {
      active = false;
      controller.abort();
    };
  }, [filter, searchApplied, reloadTick, t]);

  const loadMore = () => {
    if (loadingMore || rows.length >= total) return;
    setLoadingMore(true);
    void api
      .tempChannelsList({
        status: filter,
        ...(searchApplied ? { q: searchApplied } : {}),
        limit: PAGE_SIZE,
        offset: rows.length,
      })
      .then((res) => {
        setRows((cur) => {
          const seen = new Set(cur.map((item) => item.id));
          return [...cur, ...(res.channels ?? []).filter((item) => !seen.has(item.id))];
        });
        setTotal(res.total ?? 0);
        if (res.counts) {
          setCounts(res.counts);
          onCountsRef.current?.(res.counts);
        }
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : t("Could not load older channels."));
      })
      .finally(() => {
        setLoadingMore(false);
      });
  };

  const refreshAll = () => setReloadTick((n) => n + 1);

  if (unavailable) {
    return (
      <PageState title={t("Temp channels audit unavailable")}>
        {t("This workspace deployment does not expose temporary channel audit listing yet.")}
      </PageState>
    );
  }

  const filterButtons: { key: TempChannelAuditStatusFilter; label: string; count: number }[] = [
    { key: "all", label: t("All"), count: counts.all },
    { key: "open", label: t("Open"), count: counts.open },
    { key: "needs_human", label: t("Needs human"), count: counts.needs_human },
    { key: "ended", label: t("Closed / Expired"), count: counts.ended },
  ];
  const hasFilters = filter !== "all" || searchApplied !== "";
  const openSummary = rows.find((item) => item.id === openId) ?? null;

  return (
    <div className="space-y-3 min-w-0" data-temp-channels-tab>
      <div className="flex flex-wrap items-start justify-between gap-2 min-w-0">
        <div className="min-w-0 flex-1">
          <h2 className="text-secondary font-medium text-ink-100">{t("Temporary agent channels")}</h2>
          <p className="text-label text-ink-400 mt-0.5 break-words">
            {t("User-triggered ephemeral relays created via chat or Workspace MCP. Inspect external peer messages, override triage decisions, or close active rooms immediately.")}
          </p>
        </div>
        <Button size="sm" variant="ghost" disabled={loading} onClick={refreshAll}>
          <IconRefresh size={14} />
          {t("Refresh")}
        </Button>
      </div>

      <div className="tchan-toolbar">
        <div className="tchan-filters" role="group" aria-label={t("Channel status filter")}>
          {filterButtons.map((btn) => (
            <button
              key={btn.key}
              type="button"
              className="tchan-filter"
              aria-pressed={filter === btn.key}
              data-temp-filter={btn.key}
              onClick={() => setFilter(btn.key)}
            >
              <span>{btn.label}</span>
              <span className="num text-micro text-ink-500">({btn.count})</span>
            </button>
          ))}
        </div>

        <div className="tchan-search">
          <input
            type="search"
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            placeholder={t("Search purpose, ID, host, or peer…")}
            aria-label={t("Filter temporary channels")}
            className="field w-full"
          />
          {searchInput !== "" && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setSearchInput("");
                setSearchApplied("");
              }}
            >
              {t("Clear")}
            </Button>
          )}
        </div>
      </div>

      {error && (
        <div className="card px-3 py-2 flex flex-wrap items-center justify-between gap-2" role="alert">
          <span className="text-label text-fail break-words">{error}</span>
          <Button size="sm" onClick={refreshAll}>
            {t("Retry")}
          </Button>
        </div>
      )}

      {loading && rows.length === 0 ? (
        <p className="text-label text-ink-500 py-4">{t("Loading temporary channels…")}</p>
      ) : rows.length === 0 ? (
        <section className="settings-rows px-4 py-6 text-center" aria-label={t("Temporary channels")}>
          <p className="text-secondary text-ink-300">
            {hasFilters ? t("No channels match this filter") : t("No temporary agent channels recorded")}
          </p>
          <p className="text-label text-ink-500 mt-1">
            {hasFilters
              ? t("Adjust the status pill or clear the search query to inspect other channels.")
              : t("Channels appear when a workspace member opens an external relay. Inbound peer messages stay sandboxed for triage and expire automatically.")}
          </p>
          {hasFilters && (
            <div className="mt-3">
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  setFilter("all");
                  setSearchInput("");
                  setSearchApplied("");
                }}
              >
                {t("Reset filters")}
              </Button>
            </div>
          )}
        </section>
      ) : (
        <>
          <ul className="settings-rows" aria-label={t("Temporary channels")}>
            {rows.map((item) => {
              const badge = tempChannelStatusView(item);
              const peerText = item.peer_claimed
                ? `${item.peer_label ?? t("claimed")} (${t("locked")})`
                : t("Waiting for peer");
              const creatorText = item.creator_label ?? item.creator_user_id;
              return (
                <li key={item.id}>
                  <button
                    type="button"
                    className="settings-row"
                    data-channel-id={item.id}
                    aria-expanded={openId === item.id}
                    onClick={() => setOpenId(item.id)}
                  >
                    <span className="settings-row-text">
                      <b>{item.purpose}</b>
                      <small className="num">
                        {shortTempChannelId(item.id)} · {creatorText}
                        {item.host_label ? ` · ${t("Host")}: ${item.host_label}` : ""} ·{" "}
                        {t("Peer")}: {peerText} · {tempChannelCountsSummary(item)} ·{" "}
                        {formatChannelExpiry(item)}
                      </small>
                    </span>
                    <span className="conn-row-chips">
                      <span className={`chip ${chipToneClass(badge.tone)}`}>{t(badge.label)}</span>
                      <span className="settings-quiet">{t("Inspect")}</span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>

          <div className="tchan-footer">
            <span>
              {t("Showing {shown} of {total} channels")
                .replace("{shown}", String(rows.length))
                .replace("{total}", String(total))}
            </span>
            {rows.length < total && (
              <Button size="sm" variant="secondary" loading={loadingMore} onClick={loadMore}>
                {t("Load {count} older").replace(
                  "{count}",
                  String(Math.min(PAGE_SIZE, total - rows.length)),
                )}
              </Button>
            )}
          </div>
        </>
      )}

      {openId !== null && (
        <SettingsDrawer
          key={openId}
          title={openSummary?.purpose ?? shortTempChannelId(openId)}
          onClose={() => setOpenId(null)}
        >
          <TempChannelInspectDrawerContent
            channelId={openId}
            canWrite={canWrite}
            onChanged={refreshAll}
          />
        </SettingsDrawer>
      )}
    </div>
  );
}

function TempChannelInspectDrawerContent({
  channelId,
  canWrite,
  onChanged,
}: {
  channelId: string;
  canWrite: boolean;
  onChanged: () => void;
}) {
  const t = useTranslate();
  const [detail, setDetail] = useState<TempChannelAuditDetailPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [rotatedPrompt, setRotatedPrompt] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const [reasons, setReasons] = useState<Record<string, string>>({});

  useEffect(() => {
    const controller = new AbortController();
    let active = true;
    setLoading(true);
    setError(null);
    setConfirmClose(false);
    setNotice(null);
    setRotatedPrompt(null);

    void api
      .tempChannelDetail(channelId, controller.signal)
      .then((res) => {
        if (active) setDetail(res);
      })
      .catch((e: unknown) => {
        if (!active || (e instanceof DOMException && e.name === "AbortError")) return;
        setError(e instanceof Error ? e.message : t("Could not load channel details."));
      })
      .finally(() => {
        if (active) setLoading(false);
      });

    return () => {
      active = false;
      controller.abort();
    };
  }, [channelId, t]);

  if (loading && !detail) {
    return <p className="text-label text-ink-500">{t("Loading channel transcript…")}</p>;
  }
  if (error && !detail) {
    return (
      <p className="text-label text-fail" role="alert">
        {error}
      </p>
    );
  }
  if (!detail) return null;

  const { snapshot, creator_label, creator_user_id } = detail;
  const ch = snapshot.channel;
  const isOpen = ch.status === "open";
  const pendingCount = isOpen
    ? snapshot.messages.filter((m) => m.role === "peer" && m.triage_status === "pending").length
    : 0;
  const needsHumanCount = isOpen
    ? snapshot.messages.filter((m) => m.role === "peer" && m.triage_status === "needs_human").length
    : 0;
  const badge = tempChannelStatusView({
    status: ch.status,
    pending_count: pendingCount,
    needs_human_count: needsHumanCount,
  });

  const handleDecision = (msg: TempChannelMessage, decision: TempChannelDecision) => {
    if (!canWrite || busy) return;
    const summary =
      (reasons[msg.id] ?? "").trim() ||
      (decision === "handled"
        ? t("Marked handled by workspace member in Settings.")
        : t("Discarded by workspace member in Settings."));
    setBusy(`decide:${msg.id}`);
    setError(null);
    setNotice(null);
    void api
      .tempChannelDecide(ch.id, { message_id: msg.id, decision, summary })
      .then((res) => {
        setDetail((cur) => {
          if (!cur) return cur;
          const updatedMsgs = cur.snapshot.messages.map((item) =>
            item.id === res.target_message_id
              ? {
                  ...item,
                  triage_status: res.triage_status,
                  decision_summary: summary,
                  decided_at: res.decision_message.decided_at,
                  decided_by: res.decision_message.decided_by,
                }
              : item,
          );
          return {
            ...cur,
            snapshot: {
              ...cur.snapshot,
              channel: res.channel,
              messages: [...updatedMsgs, res.decision_message],
              latest_seq: res.channel.latest_seq,
            },
          };
        });
        setNotice(
          decision === "handled"
            ? t("Peer message marked handled.")
            : t("Peer message discarded."),
        );
        onChanged();
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : t("Failed to record decision."));
      })
      .finally(() => {
        setBusy(null);
      });
  };

  const handleRotate = () => {
    if (!canWrite || busy) return;
    setBusy("rotate");
    setError(null);
    setNotice(null);
    void api
      .tempChannelRotatePeer(ch.id)
      .then((res) => {
        setRotatedPrompt(res.peer_prompt);
        setNotice(t("Peer invite rotated; previous peer token revoked and lock reset."));
        return api.tempChannelDetail(ch.id).then((fresh) => {
          setDetail(fresh);
          onChanged();
        });
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : t("Failed to rotate peer invite."));
      })
      .finally(() => {
        setBusy(null);
      });
  };

  const handleClose = () => {
    if (!canWrite || busy) return;
    setBusy("close");
    setError(null);
    setNotice(null);
    void api
      .tempChannelClose(ch.id, { reason: "Closed via Settings kill switch." })
      .then((res) => {
        setConfirmClose(false);
        setDetail((cur) => {
          if (!cur) return cur;
          return {
            ...cur,
            snapshot: {
              ...cur.snapshot,
              channel: res.channel,
              messages: res.message ? [...cur.snapshot.messages, res.message] : cur.snapshot.messages,
              latest_seq: res.channel.latest_seq,
            },
          };
        });
        setNotice(t("Channel closed via kill switch; all capability tokens invalidated."));
        onChanged();
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : t("Failed to close channel."));
      })
      .finally(() => {
        setBusy(null);
      });
  };

  return (
    <div className="space-y-3 min-w-0" data-temp-channel-drawer={ch.id}>
      <div className="flex flex-wrap items-center gap-2">
        <span className={`chip ${chipToneClass(badge.tone)}`} data-drawer-status>
          {t(badge.label)}
        </span>
        <span className="num text-micro text-ink-500 break-all">{ch.id}</span>
      </div>

      {isOpen && canWrite && (
        <div className="flex flex-wrap items-center justify-between gap-2 card px-3 py-2">
          <Button
            size="sm"
            variant="secondary"
            loading={busy === "rotate"}
            disabled={busy !== null}
            onClick={handleRotate}
          >
            {t("Rotate peer invite")}
          </Button>

          {confirmClose ? (
            <div className="flex flex-wrap items-center gap-1.5">
              <Button
                size="sm"
                variant="danger"
                loading={busy === "close"}
                disabled={busy !== null}
                onClick={handleClose}
              >
                {t("Confirm close")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={busy !== null}
                onClick={() => setConfirmClose(false)}
              >
                {t("Cancel")}
              </Button>
            </div>
          ) : (
            <Button
              size="sm"
              variant="danger"
              disabled={busy !== null}
              onClick={() => setConfirmClose(true)}
            >
              {t("Close channel")}
            </Button>
          )}
        </div>
      )}

      {notice && (
        <div className="custmcp-ok-banner" role="status">
          {notice}
        </div>
      )}

      {error && (
        <p className="text-label text-fail" role="alert">
          {error}
        </p>
      )}

      {rotatedPrompt && (
        <div className="space-y-1.5">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="slabel">{t("New peer prompt (copy once)")}</span>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                if (typeof navigator !== "undefined" && navigator.clipboard) {
                  void navigator.clipboard.writeText(rotatedPrompt).catch(() => {});
                }
              }}
            >
              {t("Copy new peer prompt")}
            </Button>
          </div>
          <pre className="custmcp-pre" tabIndex={0}>
            {rotatedPrompt}
          </pre>
        </div>
      )}

      <dl className="tchan-meta">
        <div>
          <dt>{t("Triggered by")}</dt>
          <dd>{creator_label ?? creator_user_id ?? t("Workspace member")}</dd>
        </div>
        <div>
          <dt>{t("Lifetime")}</dt>
          <dd>{formatChannelExpiry(ch)}</dd>
        </div>
        <div>
          <dt>{t("Host session")}</dt>
          <dd className="num">{ch.host_label ?? t("host-agent")}</dd>
        </div>
        <div>
          <dt>{t("Peer lock")}</dt>
          <dd>
            {ch.peer_claimed
              ? `${ch.peer_label ?? t("external-agent")} (${t("first-claim locked")})`
              : t("Unclaimed")}
          </dd>
        </div>
        <div>
          <dt>{t("Message budget")}</dt>
          <dd className="num">
            {ch.message_count} / {ch.max_messages}
          </dd>
        </div>
        <div>
          <dt>{t("Token custody")}</dt>
          <dd>{ch.closed_reason ?? t("SHA-256 hashed at rest (never exposed)")}</dd>
        </div>
      </dl>

      <div className="conn-risk rounded border border-warn/35 bg-warn/10 px-3 py-2">
        <div className="conn-risk-title">
          <IconWarning size={14} />
          <strong>{t("Untrusted external agent data")}</strong>
        </div>
        <p className="conn-risk-desc text-ink-300">
          {snapshot.security_notice}
        </p>
      </div>

      <section className="space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="slabel">{t("Transcript & triage audit")}</h3>
          <span className="num text-micro text-ink-500">
            {snapshot.messages.length} {t("entries")}
          </span>
        </div>

        {snapshot.messages.length === 0 ? (
          <div className="card px-3 py-4 text-center">
            <p className="text-label text-ink-300">
              {isOpen
                ? t("No peer messages yet — waiting for external agent to claim the one-time invite.")
                : t("Channel ended with no messages recorded.")}
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {snapshot.messages.map((msg) => {
              const triage = msg.triage_status ? triageStatusView(msg.triage_status) : null;
              const canOverride =
                canWrite &&
                isOpen &&
                msg.role === "peer" &&
                (msg.triage_status === "pending" || msg.triage_status === "needs_human");

              return (
                <article
                  key={msg.id}
                  className={`tchan-msg${msg.role === "peer" ? " tchan-msg-peer" : ""}`}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <strong className="text-label text-ink-100">
                        {msg.sender_label ?? msg.role}
                      </strong>
                      <span className="chip bg-ink-800 text-ink-300 num">{msg.kind}</span>
                      {msg.role === "peer" && (
                        <span className="chip custmcp-chip-warn">{t("Untrusted external data")}</span>
                      )}
                      {triage && (
                        <span className={`chip ${chipToneClass(triage.tone)}`}>{t(triage.label)}</span>
                      )}
                    </div>
                    <span className="num text-micro text-ink-500">
                      #{msg.seq} · {fmtTime(new Date(msg.created_at).toISOString())}
                    </span>
                  </div>

                  <p className="tchan-msg-body">{msg.body}</p>

                  {msg.decision_summary && (
                    <div className="border-t border-ink-700 pt-1.5 text-micro text-ink-400">
                      {t("Verdict")} ({msg.decided_by ?? t("host")}): {msg.decision_summary}
                    </div>
                  )}

                  {canOverride && (
                    <div className="border-t border-ink-700 pt-2 space-y-1.5">
                      <input
                        type="text"
                        maxLength={200}
                        value={reasons[msg.id] ?? ""}
                        onChange={(e) =>
                          setReasons((cur) => ({ ...cur, [msg.id]: e.target.value }))
                        }
                        placeholder={t("Owner feedback / audit reason (optional)…")}
                        aria-label={t("Owner feedback / audit reason")}
                        className="field w-full"
                        disabled={busy !== null}
                      />
                      <div className="flex flex-wrap gap-2">
                        <Button
                          size="sm"
                          variant="primary"
                          loading={busy === `decide:${msg.id}`}
                          disabled={busy !== null}
                          onClick={() => handleDecision(msg, "handled")}
                        >
                          {t("Mark handled")}
                        </Button>
                        <Button
                          size="sm"
                          variant="danger"
                          disabled={busy !== null}
                          onClick={() => handleDecision(msg, "discarded")}
                        >
                          {t("Discard")}
                        </Button>
                      </div>
                    </div>
                  )}
                </article>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}
