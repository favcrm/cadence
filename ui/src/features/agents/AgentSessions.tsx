import { useEffect, useRef, useState, type MouseEvent } from "react";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, withPrivateOperation } from "../../lib/privateSession";
import { sessionHeaders } from "../../lib/sessionKey";
import { navigate, useHref } from "../../lib/useLocation";
import Button from "../../ui/Button";
import "./agentSessions.css";

type Session = {
  session_ref: string;
  tenant_name: string;
  agent_id: string;
  agent_name: string | null;
  source: string;
  status: string;
  started_at: number | null;
  last_activity_at: number | null;
  ended_at: number | null;
  duration_ms: number | null;
  runtime_build?: string | null;
  model?: string | null;
  failure_code?: string | null;
  ticket_id?: string | null;
  routine_id?: string | null;
};
type Message = { role: "operator" | "agent"; text: string; created?: string };
type List = {
  tenant_id: string;
  tenant_name?: string;
  sessions: Session[];
  next_cursor: string | null;
  coverage: "observed_only";
};
const refPattern = /^as_[a-f0-9]{32}$/;

async function get(path: string, signal: AbortSignal): Promise<unknown> {
  return withPrivateOperation(async (op) => {
    const response = await op.fetch(path, { headers: sessionHeaders(), signal });
    if (!response.ok) throw new Error("unavailable");
    const data: unknown = await response.json();
    op.assertCurrent();
    return data;
  }, signal);
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function session(value: unknown): value is Session {
  return (
    object(value) &&
    typeof value.session_ref === "string" &&
    refPattern.test(value.session_ref) &&
    typeof value.tenant_name === "string" &&
    (typeof value.agent_name === "string" || value.agent_name === null) &&
    typeof value.agent_id === "string" &&
    typeof value.source === "string" &&
    typeof value.status === "string"
  );
}
function list(value: unknown): value is List {
  return (
    object(value) &&
    typeof value.tenant_id === "string" &&
    (value.tenant_name === undefined || typeof value.tenant_name === "string") &&
    value.coverage === "observed_only" &&
    Array.isArray(value.sessions) &&
    value.sessions.every(session) &&
    (value.next_cursor === null || typeof value.next_cursor === "string")
  );
}
function transcript(value: unknown): value is { messages: Message[]; next_cursor: string | null } {
  return (
    object(value) &&
    Array.isArray(value.messages) &&
    value.messages.every(
      (m: unknown) =>
        object(m) &&
        (m.role === "operator" || m.role === "agent") &&
        typeof m.text === "string" &&
        (m.created === undefined || typeof m.created === "string"),
    ) &&
    (value.next_cursor === null || typeof value.next_cursor === "string")
  );
}
function date(value: number | null | undefined): string {
  return typeof value === "number" &&
    Number.isFinite(value) &&
    !Number.isNaN(new Date(value).getTime())
    ? new Date(value)
        .toISOString()
        .replace("T", " ")
        .replace(/(?:\.000)?Z$/, " UTC")
    : "—";
}
function duration(value: number | null | undefined): string {
  return typeof value === "number" && Number.isFinite(value)
    ? `${(value / 1000).toFixed(1)} s`
    : "—";
}

export default function AgentSessions() {
  const t = useTranslate();
  const href = useHref();
  const params = new URL(href, location.origin).searchParams;
  const selected = params.get("sessions") || "list";
  const query = params.get("q") || "";
  const tenant = params.get("tenant") || "";
  const status = params.get("status") || "";
  const cursors = params.getAll("cursor");
  const cursor = cursors.at(-1) || "";
  const focus = params.get("focus");
  const [rows, setRows] = useState<List | null>(null);
  const [tenantOption, setTenantOption] = useState<{ id: string; name: string } | null>(null);
  const [detail, setDetail] = useState<Session | null>(null);
  const [listState, setListState] = useState<"loading" | "ready" | "error">("loading");
  const [detailState, setDetailState] = useState<"loading" | "ready" | "error">("loading");
  const [messages, setMessages] = useState<Message[]>([]);
  const [messageCursor, setMessageCursor] = useState<string | null>(null);
  const [messageState, setMessageState] = useState<"loading" | "ready" | "error">("loading");
  const [copyState, setCopyState] = useState("");
  const heading = useRef<HTMLHeadingElement>(null);
  const transcriptHeading = useRef<HTMLHeadingElement>(null);
  const messageRequest = useRef<AbortController | null>(null);
  useEffect(
    () =>
      onPrivateReadsPaused(() => {
        setRows(null);
        setTenantOption(null);
        setDetail(null);
        setMessages([]);
        setMessageCursor(null);
        setListState("error");
        setDetailState("error");
        setMessageState("error");
      }),
    [],
  );
  const update = (changes: Record<string, string | null>, replace = false) => {
    const next = new URL(location.href);
    for (const [key, value] of Object.entries(changes)) {
      if (value === null) next.searchParams.delete(key);
      else next.searchParams.set(key, value);
    }
    navigate(next.pathname + next.search, { replace });
  };
  const changeCriteria = (key: string, value: string) => {
    const next = new URL(location.href);
    if (value) next.searchParams.set(key, value);
    else next.searchParams.delete(key);
    next.searchParams.delete("cursor");
    next.searchParams.delete("focus");
    navigate(next.pathname + next.search, { replace: true });
  };
  useEffect(() => {
    if (selected !== "list") return;
    const controller = new AbortController();
    setRows(null);
    setListState("loading");
    const p = new URLSearchParams({ limit: "25" });
    if (query) p.set("q", query);
    if (tenant) p.set("tenant", tenant);
    if (status) p.set("status", status);
    if (cursor) p.set("cursor", cursor);
    get(`/api/agent-sessions?${p}`, controller.signal)
      .then((data) => {
        if (!list(data)) throw new Error("invalid");
        const label = data.tenant_name ?? data.sessions[0]?.tenant_name;
        if (label) setTenantOption({ id: data.tenant_id, name: label });
        setRows(data);
        setListState("ready");
      })
      .catch(() => {
        if (!controller.signal.aborted) setListState("error");
      });
    return () => controller.abort();
  }, [selected, query, tenant, status, cursor]);
  useEffect(() => {
    if (selected !== "list" || listState !== "ready" || !focus) return;
    document.getElementById(`inspect-${focus}`)?.focus();
  }, [selected, listState, focus]);
  useEffect(() => {
    if (selected === "list") return;
    const controller = new AbortController();
    setDetail(null);
    setDetailState("loading");
    setMessages([]);
    setMessageCursor(null);
    setMessageState("loading");
    setCopyState("");
    if (!refPattern.test(selected)) {
      setDetailState("error");
      return () => controller.abort();
    }
    get(`/api/agent-sessions/${encodeURIComponent(selected)}`, controller.signal)
      .then((data) => {
        if (!object(data) || !session(data.session) || data.session.session_ref !== selected)
          throw new Error("invalid");
        setDetail(data.session);
        setDetailState("ready");
      })
      .catch(() => {
        if (!controller.signal.aborted) setDetailState("error");
      });
    return () => controller.abort();
  }, [selected]);
  useEffect(() => {
    if (detailState !== "ready" || !detail || detail.session_ref !== selected) return;
    const controller = new AbortController();
    messageRequest.current = controller;
    get(`/api/agent-sessions/${encodeURIComponent(selected)}/transcript`, controller.signal)
      .then((data) => {
        if (!transcript(data)) throw new Error("invalid");
        setMessages(data.messages);
        setMessageCursor(data.next_cursor);
        setMessageState("ready");
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setMessages([]);
          setMessageState("error");
        }
      });
    heading.current?.focus();
    return () => {
      controller.abort();
      messageRequest.current?.abort();
    };
  }, [selected, detailState, detail]);
  const loadMore = () => {
    if (!messageCursor || messageState !== "ready") return;
    const controller = new AbortController();
    messageRequest.current = controller;
    setMessageState("loading");
    get(
      `/api/agent-sessions/${encodeURIComponent(selected)}/transcript?${new URLSearchParams({ cursor: messageCursor })}`,
      controller.signal,
    )
      .then((data) => {
        if (!transcript(data)) throw new Error("invalid");
        setMessages((old) => [...old, ...data.messages]);
        setMessageCursor(data.next_cursor);
        setMessageState("ready");
      })
      .catch(() => {
        if (!controller.signal.aborted) {
          setMessages([]);
          setMessageCursor(null);
          setMessageState("error");
        }
      });
  };
  const back = () => {
    if (history.state?.agentSessionsFromList) history.back();
    else update({ sessions: "list" }, true);
  };
  const inspect = (event: MouseEvent<HTMLAnchorElement>, id: string) => {
    if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    const listUrl = new URL(location.href);
    listUrl.searchParams.set("focus", id);
    history.replaceState(history.state, "", listUrl.pathname + listUrl.search);
    const target = new URL(listUrl);
    target.searchParams.set("sessions", id);
    navigate(target.pathname + target.search);
    history.replaceState({ ...history.state, agentSessionsFromList: true }, "");
  };
  const page = (nextCursor: string | null) => {
    const url = new URL(location.href);
    url.searchParams.delete("cursor");
    url.searchParams.delete("focus");
    const stack = nextCursor ? [...cursors, nextCursor] : cursors.slice(0, -1);
    stack.forEach((c) => url.searchParams.append("cursor", c));
    navigate(url.pathname + url.search, { replace: true });
  };
  if (selected !== "list")
    return (
      <main className="agent-sessions">
        <Button onClick={back}>{t("← Back to sessions")}</Button>
        {detailState === "loading" && <p role="status">{t("Loading session…")}</p>}
        {detailState === "error" && <h1>{t("Session unavailable")}</h1>}
        {detailState === "ready" && detail && detail.session_ref === selected && (
          <>
            <header className="as-head">
              <div>
                <p>{t("Session details")}</p>
                <h1 tabIndex={-1} ref={heading}>
                  {detail.session_ref}
                </h1>
                <p>
                  {detail.agent_name ?? detail.agent_id} · {detail.tenant_name} · {t(detail.status)}
                </p>
              </div>
              <div className="as-actions">
                {messageState === "ready" && (messages.length > 0 || messageCursor !== null) && (
                  <Button
                    onClick={() => {
                      transcriptHeading.current?.focus();
                      transcriptHeading.current?.scrollIntoView({ block: "start" });
                    }}
                  >
                    {t("View transcript")}
                  </Button>
                )}
                <Button
                  onClick={() => {
                    const text = [
                      `${t("Session")}: ${detail.session_ref}`,
                      `${t("Tenant")}: ${detail.tenant_name}`,
                      `${t("Agent")}: ${detail.agent_name ?? detail.agent_id}${detail.agent_name ? ` (${detail.agent_id})` : ""}`,
                      `${t("Status")}: ${detail.status}`,
                      `${t("Source")}: ${detail.source}`,
                      `${t("Started (UTC)")}: ${date(detail.started_at)}`,
                      `${t("Duration")}: ${duration(detail.duration_ms)}`,
                      `${t("Runtime build")}: ${detail.runtime_build || "—"}`,
                      `${t("Model")}: ${detail.model || "—"}`,
                      `${t("Related work")}: ${detail.ticket_id || detail.routine_id || "—"}`,
                      `${t("Failure code")}: ${detail.failure_code || "—"}`,
                    ].join("\n");
                    if (!navigator.clipboard) {
                      setCopyState(t("Copy unavailable"));
                      return;
                    }
                    void navigator.clipboard.writeText(text).then(
                      () => setCopyState(t("Summary copied")),
                      () => setCopyState(t("Copy unavailable")),
                    );
                  }}
                >
                  {t("Copy summary")}
                </Button>
                <span role="status">{copyState}</span>
              </div>
            </header>
            <dl className="as-facts">
              {[
                [t("Source"), t(detail.source)],
                [t("Related work"), detail.ticket_id || detail.routine_id || "—"],
                [t("Started (UTC)"), date(detail.started_at)],
                [t("Last activity (UTC)"), date(detail.last_activity_at)],
                [t("Ended (UTC)"), date(detail.ended_at)],
                [t("Duration"), duration(detail.duration_ms)],
                [t("Runtime build"), detail.runtime_build || "—"],
                [t("Model"), detail.model || "—"],
                [t("Failure code"), detail.failure_code || "—"],
              ].map(([label, value]) => (
                <div key={label}>
                  <dt>{label}</dt>
                  <dd>{value}</dd>
                </div>
              ))}
            </dl>
            <section className="as-transcript">
              <h2 ref={transcriptHeading} tabIndex={-1}>
                {t("Transcript")}
              </h2>
              {messageState === "error" ? (
                <p>{t("Transcript unavailable")}</p>
              ) : (
                <>
                  <p>{t("Read-only · User-visible messages · Oldest first")}</p>
                  <ol>
                    {messages.map((m, i) => (
                      <li key={i}>
                        <div>
                          <strong>{t(m.role === "operator" ? "User" : "Agent")}</strong>
                          {m.created && (
                            <time dateTime={m.created}>{date(Date.parse(m.created))}</time>
                          )}
                        </div>
                        <p>{m.text}</p>
                      </li>
                    ))}
                  </ol>
                  {messageState === "loading" && <p role="status">{t("Loading transcript…")}</p>}
                  {messageState === "ready" && messages.length === 0 && (
                    <p>{t("No messages recorded.")}</p>
                  )}
                  {messageState === "ready" && messageCursor && (
                    <Button onClick={loadMore}>{t("Load more")}</Button>
                  )}
                </>
              )}
            </section>
          </>
        )}
      </main>
    );
  return (
    <main className="agent-sessions">
      <h1>{t("Agent sessions")}</h1>
      <p>{t("Search recent sessions and inspect the outcome.")}</p>
      <div className="as-filters" role="search">
        <label>
          {t("Search sessions")}
          <input
            type="search"
            maxLength={200}
            value={query}
            placeholder={t("Tenant, agent or session ID")}
            onChange={(e) => changeCriteria("q", e.target.value)}
          />
        </label>
        <label>
          {t("Tenant")}
          <select value={tenant} onChange={(e) => changeCriteria("tenant", e.target.value)}>
            <option value="">{t("All tenants")}</option>
            {tenantOption && <option value={tenantOption.id}>{tenantOption.name}</option>}
          </select>
        </label>
        <label>
          {t("Status")}
          <select value={status} onChange={(e) => changeCriteria("status", e.target.value)}>
            <option value="">{t("All statuses")}</option>
            {["open", "reset", "finished", "failed", "stopped"].map((s) => (
              <option key={s} value={s}>
                {t(s)}
              </option>
            ))}
          </select>
        </label>
        <Button
          onClick={() => {
            const url = new URL(location.href);
            ["q", "tenant", "status", "cursor", "focus"].forEach((key) =>
              url.searchParams.delete(key),
            );
            navigate(url.pathname + url.search, { replace: true });
          }}
        >
          {t("Clear filters")}
        </Button>
      </div>
      {listState === "loading" && <p role="status">{t("Loading sessions…")}</p>}
      {listState === "error" && <p role="status">{t("Sessions unavailable")}</p>}
      {listState === "ready" && rows && (
        <>
          <div
            className="as-table"
            role="region"
            aria-label={t("Agent session results")}
            tabIndex={0}
          >
            <table>
              <thead>
                <tr>
                  {[
                    "Agent / session",
                    "Tenant",
                    "Status",
                    "Source",
                    "Started (UTC)",
                    "Duration",
                    "Inspect",
                  ].map((label, i) => (
                    <th scope="col" className={i >= 3 && i <= 5 ? "as-secondary" : ""} key={label}>
                      {t(label)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {rows.sessions.map((row) => {
                  const url = new URL(location.href);
                  url.searchParams.set("sessions", row.session_ref);
                  return (
                    <tr key={row.session_ref}>
                      <th scope="row">
                        {row.agent_name ?? row.agent_id}
                        <code>{row.session_ref}</code>
                      </th>
                      <td>{row.tenant_name}</td>
                      <td>{t(row.status)}</td>
                      <td className="as-secondary">{t(row.source)}</td>
                      <td className="as-secondary">{date(row.started_at)}</td>
                      <td className="as-secondary">{duration(row.duration_ms)}</td>
                      <td>
                        <a
                          id={`inspect-${row.session_ref}`}
                          href={url.pathname + url.search}
                          onClick={(e) => inspect(e, row.session_ref)}
                          aria-label={t("Inspect {id}").replace("{id}", row.session_ref)}
                        >
                          {t("Inspect")}
                        </a>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          {rows.sessions.length === 0 && (
            <p>{t("No sessions match. Try a different search or clear the filters.")}</p>
          )}
          <nav className="as-pagination" aria-label={t("Session pages")}>
            <Button disabled={!cursors.length} onClick={() => page(null)}>
              {t("Previous")}
            </Button>
            <span>{t("Page {page}").replace("{page}", String(cursors.length + 1))}</span>
            <Button disabled={!rows.next_cursor} onClick={() => page(rows.next_cursor)}>
              {t("Next")}
            </Button>
          </nav>
          <p>{t("Only observed sessions are shown.")}</p>
        </>
      )}
    </main>
  );
}
