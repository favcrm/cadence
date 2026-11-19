import { useMemo, useState } from "react";
import { inboxApi } from "./inboxApi";
import { useTranslate } from "../../lib/locale";
import type { AgentInboxFeed, AgentInboxMessage } from "../../lib/types";
import Button from "../../ui/Button";

type InboxFilter = "all" | "colleagues" | "specialists" | "unread";

function relTime(epochSecs: number): string {
  const delta = Math.max(0, Math.floor(Date.now() / 1000 - epochSecs));
  if (delta < 60) return "just now";
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  return `${Math.floor(delta / 86400)}d ago`;
}

function kindBadgeClass(kind: string): string {
  switch (kind) {
    case "handoff":
      return "bg-accent/15 text-accent";
    case "checkin":
      return "bg-ink-800 text-ink-300";
    case "decision":
      return "bg-warn/15 text-warn";
    case "reply":
      return "bg-ok/15 text-ok";
    default:
      return "bg-ink-800 text-ink-400";
  }
}

export default function InboxPanel({
  feed,
  loading,
  error,
  readOnly,
  onRefresh,
  onOpenIssue,
}: {
  feed: AgentInboxFeed | null;
  loading: boolean;
  error: string | null;
  readOnly: boolean;
  onRefresh: () => Promise<void> | void;
  onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const [filter, setFilter] = useState<InboxFilter>("all");
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [replyText, setReplyText] = useState("");
  const [composerOpen, setComposerOpen] = useState(false);
  const [sendTo, setSendTo] = useState("");
  const [sendKind, setSendKind] = useState<"checkin" | "handoff">("checkin");
  const [sendArtifact, setSendArtifact] = useState("");
  const [sendText, setSendText] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);

  const directory = useMemo(() => {
    if (!feed) return [];
    return [...feed.directory.colleagues, ...feed.directory.specialists];
  }, [feed]);

  const messages = feed?.messages ?? [];
  const filtered = useMemo(() => {
    return messages.filter((m) => {
      if (filter === "colleagues") return m.sender_kind === "colleague";
      if (filter === "specialists") return m.sender_kind === "specialist";
      if (filter === "unread") return m.unread;
      return true;
    });
  }, [messages, filter]);

  const effectiveTarget = sendTo || directory[0]?.alias || "";

  const handleAck = async (through: number, key: string) => {
    if (readOnly || busyId) return;
    setBusyId(key);
    setActionError(null);
    setActionNotice(null);
    try {
      await inboxApi.ack({ alias: feed?.alias, through });
      await onRefresh();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : t("Could not update inbox cursor."));
    } finally {
      setBusyId(null);
    }
  };

  const handleReply = async (msg: AgentInboxMessage) => {
    const text = replyText.trim();
    if (readOnly || busyId || !text) return;
    const target = msg.reply_to || msg.sender;
    if (!target || target === "unknown" || target === "operator") {
      setActionError(t("No reply target alias on this message."));
      return;
    }
    setBusyId(`reply:${msg.id}`);
    setActionError(null);
    setActionNotice(null);
    try {
      await inboxApi.send({
        to: target,
        text,
        kind: "reply",
        reply_to: feed?.alias,
      });
      if (msg.unread) {
        await inboxApi.ack({ alias: feed?.alias, through: msg.seq });
      }
      setReplyText("");
      setActionNotice(t("Reply sent to {target}.").replace("{target}", target));
      await onRefresh();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : t("Could not send reply."));
    } finally {
      setBusyId(null);
    }
  };

  const handlePromote = async (msg: AgentInboxMessage) => {
    if (readOnly || busyId) return;
    setBusyId(`promote:${msg.id}`);
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await inboxApi.promote({ alias: feed?.alias, message: msg.id });
      setActionNotice(
        res.already_promoted
          ? t("Already tracked as {id}.").replace("{id}", res.issue)
          : t("Created {id} from inbox message.").replace("{id}", res.issue),
      );
      await onRefresh();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : t("Could not create board issue."));
    } finally {
      setBusyId(null);
    }
  };

  const handleDecide = async (msg: AgentInboxMessage, decision: "approved" | "declined") => {
    if (readOnly || busyId) return;
    setBusyId(`decide:${msg.id}:${decision}`);
    setActionError(null);
    setActionNotice(null);
    try {
      await inboxApi.decide({ alias: feed?.alias, message: msg.id, decision });
      if (msg.unread) {
        await inboxApi.ack({ alias: feed?.alias, through: msg.seq });
      }
      setActionNotice(
        decision === "approved"
          ? t("Approved proposal from {sender}.").replace("{sender}", msg.sender)
          : t("Declined proposal from {sender}.").replace("{sender}", msg.sender),
      );
      await onRefresh();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : t("Could not record decision."));
    } finally {
      setBusyId(null);
    }
  };

  const handleSend = async (e: React.FormEvent) => {
    e.preventDefault();
    const text = sendText.trim();
    if (readOnly || busyId || !effectiveTarget || !text) return;
    setBusyId("compose");
    setActionError(null);
    setActionNotice(null);
    try {
      await inboxApi.send({
        to: effectiveTarget,
        text,
        kind: sendKind,
        reply_to: feed?.alias,
        ...(sendArtifact.trim() ? { artifact: sendArtifact.trim() } : {}),
      });
      setSendText("");
      setSendArtifact("");
      setComposerOpen(false);
      setActionNotice(t("Delivered to {target} (no ticket created).").replace("{target}", effectiveTarget));
      await onRefresh();
    } catch (e) {
      setActionError(e instanceof Error ? e.message : t("Could not deliver message."));
    } finally {
      setBusyId(null);
    }
  };

  if (loading && !feed) {
    return <p className="px-3 py-2.5 text-label text-ink-500">{t("Loading Agent Inbox…")}</p>;
  }

  if (error && !feed) {
    return (
      <p className="px-3 py-2.5 text-label text-fail break-words" role="alert">
        {error}
      </p>
    );
  }

  return (
    <div className="flex flex-col min-w-0" data-agent-inbox>
      <div className="px-3 py-2 border-b border-ink-800 flex flex-wrap items-center justify-between gap-1.5">
        <div className="flex flex-wrap items-center gap-1" role="group" aria-label={t("Inbox filter")}>
          {(
            [
              ["all", t("All")],
              ["colleagues", t("Colleagues")],
              ["specialists", t("Specialists")],
              ["unread", t("Unread")],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              type="button"
              aria-pressed={filter === key}
              onClick={() => setFilter(key)}
              className={`px-2 py-0.5 rounded text-micro transition-colors ${
                filter === key
                  ? "bg-ink-700 text-ink-100 font-medium"
                  : "text-ink-400 hover:text-ink-200 hover:bg-ink-800/70"
              }`}
            >
              {label}
              {key === "unread" && (feed?.unread ?? 0) > 0 ? ` (${feed?.unread})` : ""}
            </button>
          ))}
        </div>
        <div className="flex items-center gap-1.5">
          {!readOnly && directory.length > 0 && (
            <Button
              size="sm"
              variant={composerOpen ? "primary" : "secondary"}
              onClick={() => setComposerOpen((open) => !open)}
            >
              {composerOpen ? t("Cancel") : t("Message")}
            </Button>
          )}
          {!readOnly && (feed?.unread ?? 0) > 0 && feed && feed.tail_seq > feed.cursor && (
            <button
              type="button"
              className="lnk text-micro"
              disabled={busyId !== null}
              onClick={() => void handleAck(feed.tail_seq, "ack-all")}
            >
              {t("Mark all read")}
            </button>
          )}
        </div>
      </div>

      {composerOpen && !readOnly && (
        <form onSubmit={(e) => void handleSend(e)} className="px-3 py-2.5 border-b border-ink-800 bg-ink-900/40 space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <select
              aria-label={t("Recipient")}
              value={effectiveTarget}
              onChange={(e) => setSendTo(e.target.value)}
              className="flex-1 min-w-[140px] rounded border border-ink-700 bg-ink-950 px-2 py-1 text-micro text-ink-100"
            >
              {feed && feed.directory.colleagues.length > 0 && (
                <optgroup label={t("Colleague assistants")}>
                  {feed.directory.colleagues.map((entry) => (
                    <option key={entry.alias} value={entry.alias}>
                      {entry.alias} ({entry.role})
                    </option>
                  ))}
                </optgroup>
              )}
              {feed && feed.directory.specialists.length > 0 && (
                <optgroup label={t("Specialist agents")}>
                  {feed.directory.specialists.map((entry) => (
                    <option key={entry.alias} value={entry.alias}>
                      {entry.alias} ({entry.role})
                    </option>
                  ))}
                </optgroup>
              )}
            </select>
            <select
              aria-label={t("Message kind")}
              value={sendKind}
              onChange={(e) => setSendKind(e.target.value as "checkin" | "handoff")}
              className="rounded border border-ink-700 bg-ink-950 px-2 py-1 text-micro text-ink-100"
            >
              <option value="checkin">{t("Check-in")}</option>
              <option value="handoff">{t("Handoff")}</option>
            </select>
          </div>
          {sendKind === "handoff" && (
            <input
              type="text"
              value={sendArtifact}
              onChange={(e) => setSendArtifact(e.target.value)}
              placeholder={t("Optional artifact ref (e.g. docs/forecast.md)")}
              className="w-full rounded border border-ink-700 bg-ink-950 px-2 py-1 text-micro text-ink-100 placeholder:text-ink-500"
            />
          )}
          <textarea
            rows={2}
            value={sendText}
            onChange={(e) => setSendText(e.target.value)}
            placeholder={t("Send a lightweight check-in or handoff (no board ticket created)…")}
            className="w-full rounded border border-ink-700 bg-ink-950 px-2 py-1.5 text-label text-ink-100 placeholder:text-ink-500 resize-y"
          />
          <div className="flex items-center justify-between gap-2">
            <span className="text-micro text-ink-500">{t("No ticket created · write actions stay approval-gated")}</span>
            <Button
              size="sm"
              variant="primary"
              type="submit"
              loading={busyId === "compose"}
              disabled={busyId !== null || !sendText.trim() || !effectiveTarget}
            >
              {t("Send")}
            </Button>
          </div>
        </form>
      )}

      {actionError && (
        <p className="px-3 py-1.5 text-micro text-fail border-b border-ink-800 break-words" role="alert">
          {actionError}
        </p>
      )}
      {actionNotice && (
        <p className="px-3 py-1.5 text-micro text-ok border-b border-ink-800 break-words" role="status">
          {actionNotice}
        </p>
      )}

      {filtered.length === 0 ? (
        <div className="px-3 py-4 text-label text-ink-400 space-y-1">
          <p>
            {filter === "unread"
              ? t("No unread inbox messages.")
              : t("No assistant or specialist messages yet.")}
          </p>
          {directory.length > 0 && (
            <p className="text-micro text-ink-500">
              {t("Reachable in workspace: {count} ({colleagues} colleagues, {specialists} specialists)")
                .replace("{count}", String(directory.length))
                .replace("{colleagues}", String(feed?.directory.colleagues.length ?? 0))
                .replace("{specialists}", String(feed?.directory.specialists.length ?? 0))}
            </p>
          )}
        </div>
      ) : (
        <ul className="divide-y divide-ink-800" data-inbox-list>
          {filtered.map((msg) => {
            const expanded = expandedId === msg.id;
            return (
              <li key={msg.id} className="px-3 py-2.5 min-w-0">
                <button
                  type="button"
                  aria-expanded={expanded}
                  onClick={() => {
                    setExpandedId(expanded ? null : msg.id);
                    setReplyText("");
                  }}
                  className="w-full text-left flex flex-col gap-1 min-w-0"
                >
                  <div className="flex items-center justify-between gap-2 min-w-0">
                    <span className="flex items-center gap-1.5 min-w-0">
                      {msg.unread && (
                        <span
                          className="w-1.5 h-1.5 rounded-full bg-accent shrink-0"
                          aria-label={t("Unread")}
                        />
                      )}
                      <span className="text-label font-medium text-ink-100 truncate">
                        {msg.sender}
                      </span>
                      <span className={`chip ${kindBadgeClass(msg.kind)} shrink-0`}>
                        {msg.kind}
                      </span>
                    </span>
                    <span className="text-micro text-ink-500 num shrink-0">
                      #{msg.seq} · {relTime(msg.created)}
                    </span>
                  </div>
                  <p className="text-label text-ink-300 line-clamp-2 break-words">{msg.body}</p>
                  <div className="flex flex-wrap items-center gap-1.5 text-micro text-ink-500">
                    <span>{msg.sender_kind}</span>
                    {msg.artifact && (
                      <span className="chip bg-ink-800 text-ink-300 truncate max-w-[180px]">
                        {msg.artifact}
                      </span>
                    )}
                    {msg.auto_answered && (
                      <span className="chip bg-ok/10 text-ok">{t("auto-replied")}</span>
                    )}
                    {msg.decision_state === "pending" && (
                      <span className="chip bg-warn/15 text-warn">{t("needs approval")}</span>
                    )}
                    {msg.decision_state === "approved" && (
                      <span className="chip bg-ok/15 text-ok">{t("approved")}</span>
                    )}
                    {msg.decision_state === "declined" && (
                      <span className="chip bg-fail/15 text-fail">{t("declined")}</span>
                    )}
                    {msg.issue ? (
                      <span className="chip bg-accent/15 text-accent num">{msg.issue}</span>
                    ) : (
                      <span className="text-ink-500">{t("no ticket")}</span>
                    )}
                  </div>
                </button>

                {expanded && (
                  <div className="mt-2 pt-2 border-t border-ink-800/80 space-y-2">
                    <pre className="whitespace-pre-wrap break-words font-sans text-label text-ink-200 bg-ink-900/50 rounded p-2">
                      {msg.body}
                    </pre>
                    {msg.issue && (
                      <div className="flex items-center gap-2 text-micro">
                        <span className="text-ink-400">{t("Tracked issue:")}</span>
                        <button
                          type="button"
                          className="lnk num"
                          onClick={() => onOpenIssue(msg.issue!)}
                        >
                          {msg.issue}
                        </button>
                      </div>
                    )}
                    {!readOnly && (
                      <>
                        {msg.kind === "decision" && msg.decision_state === "pending" && (
                          <div className="flex flex-wrap items-center gap-1.5" data-inbox-decision>
                            <Button
                              size="sm"
                              variant="primary"
                              loading={busyId === `decide:${msg.id}:approved`}
                              disabled={busyId !== null}
                              onClick={() => void handleDecide(msg, "approved")}
                            >
                              {t("Approve")}
                            </Button>
                            <Button
                              size="sm"
                              variant="secondary"
                              loading={busyId === `decide:${msg.id}:declined`}
                              disabled={busyId !== null}
                              onClick={() => void handleDecide(msg, "declined")}
                            >
                              {t("Decline")}
                            </Button>
                            <span className="text-micro text-ink-500">
                              {t("Approval required before write")}
                            </span>
                          </div>
                        )}
                        <div className="flex items-center gap-1.5">
                          <input
                            type="text"
                            value={replyText}
                            onChange={(e) => setReplyText(e.target.value)}
                            placeholder={t("Reply to {sender}…").replace(
                              "{sender}",
                              msg.reply_to || msg.sender,
                            )}
                            className="flex-1 min-w-0 rounded border border-ink-700 bg-ink-950 px-2 py-1 text-micro text-ink-100 placeholder:text-ink-500"
                          />
                          <Button
                            size="sm"
                            variant="primary"
                            loading={busyId === `reply:${msg.id}`}
                            disabled={busyId !== null || !replyText.trim()}
                            onClick={() => void handleReply(msg)}
                          >
                            {t("Reply")}
                          </Button>
                        </div>
                        <div className="flex flex-wrap items-center gap-1.5">
                          {msg.unread && (
                            <Button
                              size="sm"
                              variant="secondary"
                              loading={busyId === `ack:${msg.id}`}
                              disabled={busyId !== null}
                              onClick={() => void handleAck(msg.seq, `ack:${msg.id}`)}
                            >
                              {t("Mark read")}
                            </Button>
                          )}
                          {!msg.issue && (
                            <Button
                              size="sm"
                              variant="secondary"
                              loading={busyId === `promote:${msg.id}`}
                              disabled={busyId !== null}
                              onClick={() => void handlePromote(msg)}
                            >
                              {t("Create issue")}
                            </Button>
                          )}
                        </div>
                      </>
                    )}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
