import { useCallback, useEffect, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { fmtTime } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import { fmtInZone, useTimeZones } from "../../lib/timeZone";
import type {
  AgentEndpoint,
  AgentEndpointAuthMode,
  AgentEndpointTargetKind,
  EndpointDelivery,
  Routine,
} from "../../lib/types";
import Button from "../../ui/Button";
import DrawerShell, { type DrawerMenuItem } from "../app-shell/shared/DrawerShell";
import { MASTER } from "../home/master";
import type { TabContext } from "./AgentTabs";
import { endpointsApi } from "./endpointsApi";

const ENDPOINT_SLUG_RE = /^[a-z0-9](?:[a-z0-9-]{0,18}[a-z0-9])?$/;
const DEFAULT_WEBHOOK_BASE = "https://hooks.cadencecloud.app/e/";

export function validEndpointSlug(slug: string): boolean {
  return ENDPOINT_SLUG_RE.test(slug.trim().toLowerCase());
}

/**
 * Derive `<company>` and mail domain from the board host (`<slug>.cadencecloud.app`
 * or `<slug>-staging.cadencecloud.app`) so email previews match Cloudflare Email
 * Routing (`<endpoint>.<company>@cadencecloud.app`).
 */
export function companyEmailHost(hostname?: string): { company: string; domain: string } {
  const host = (hostname ?? (typeof window !== "undefined" ? window.location.hostname : ""))
    .trim()
    .toLowerCase();
  const domain = "cadencecloud.app";
  if (host.endsWith(`.${domain}`)) {
    const label = host.slice(0, -(domain.length + 1));
    if (label && !label.includes(".")) {
      return { company: label, domain };
    }
  }
  return { company: "company", domain };
}

export function formatEndpointEmail(slug: string, hostname?: string): string {
  const { company, domain } = companyEmailHost(hostname);
  const cleanSlug = slug.trim().toLowerCase() || "endpoint";
  return `${cleanSlug}.${company}@${domain}`;
}

export function parseSenderAllowlist(raw: string): string[] {
  return raw
    .split(/[\n,]/)
    .map((entry) => entry.trim().toLowerCase())
    .filter(Boolean);
}

export function buildWebhookCurlSnippet(
  endpoint: AgentEndpoint,
  revealedSecret: string | null,
): string {
  const url = endpoint.webhook_url || `${DEFAULT_WEBHOOK_BASE}${endpoint.id}`;
  if (endpoint.auth_mode === "hmac_sha256") {
    const secretValue = revealedSecret ?? "<HMAC_SECRET>";
    return [
      `TS=$(date +%s)`,
      `BODY='{"event":"order.paid","id":"evt_1"}'`,
      `SIG=$(printf '%s.%s' "$TS" "$BODY" | openssl dgst -sha256 -hmac '${secretValue}' -hex | sed 's/^.* //')`,
      `curl -X POST '${url}' \\`,
      `  -H 'Content-Type: application/json' \\`,
      `  -H "X-Timestamp: $TS" \\`,
      `  -H "X-Signature-256: sha256=$SIG" \\`,
      `  -H 'Idempotency-Key: evt_1' \\`,
      `  --data "$BODY"`,
    ].join("\n");
  }
  const tokenValue = revealedSecret ?? "<BEARER_TOKEN>";
  return [
    `curl -X POST '${url}' \\`,
    `  -H 'Content-Type: application/json' \\`,
    `  -H 'Authorization: Bearer ${tokenValue}' \\`,
    `  -H 'Idempotency-Key: evt_1' \\`,
    `  --data '{"event":"lead.created","id":"evt_1"}'`,
  ].join("\n");
}

function endpointAddress(ep: AgentEndpoint): string {
  if (ep.kind === "email") return formatEndpointEmail(ep.slug);
  return ep.webhook_url || `${DEFAULT_WEBHOOK_BASE}${ep.id}`;
}

function endpointStatusMeta(ep: AgentEndpoint): { label: string; tone: "ok" | "warn" | "ink" } {
  if (!ep.verified) return { label: "Disabled · Unverified", tone: "warn" };
  switch (ep.state) {
    case "enabled":
      return { label: "Active", tone: "ok" };
    case "paused":
      return { label: "Paused", tone: "warn" };
    default:
      return { label: "Disabled", tone: "ink" };
  }
}

function deliveryOutcomeTone(outcome: EndpointDelivery["outcome"]): "ok" | "warn" | "ink" {
  switch (outcome) {
    case "admitted":
      return "ok";
    case "held":
    case "pending":
      return "warn";
    default:
      return "ink";
  }
}

export function IncomingEndpointsSection({
  c,
  eventRoutines,
  onEndpointsChange,
}: {
  c: TabContext;
  eventRoutines: Routine[];
  onEndpointsChange?: (endpoints: AgentEndpoint[]) => void;
}) {
  const t = useTranslate();
  const zones = useTimeZones();
  const viewerZone = zones.effective;
  const isMaster = c.alias === MASTER;

  const [endpoints, setEndpoints] = useState<AgentEndpoint[] | null>(null);
  const [notServed, setNotServed] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);

  // Add drawers
  const [createKind, setCreateKind] = useState<"email" | "webhook" | null>(null);
  const [newSlug, setNewSlug] = useState("");
  const [newAuthMode, setNewAuthMode] = useState<AgentEndpointAuthMode>("hmac_sha256");
  const [newRateLimit, setNewRateLimit] = useState("60");
  const [newTargetKind, setNewTargetKind] = useState<AgentEndpointTargetKind>("inbox");
  const [newRoutineId, setNewRoutineId] = useState("");
  const [newAllowlist, setNewAllowlist] = useState("");
  const [createErr, setCreateErr] = useState<string | null>(null);

  // Detail drawer + ephemeral one-time secret reveal
  const [openId, setOpenId] = useState<string | null>(null);
  const [detail, setDetail] = useState<AgentEndpoint | null>(null);
  const [deliveries, setDeliveries] = useState<EndpointDelivery[]>([]);
  const [revealedSecret, setRevealedSecret] = useState<string | null>(null);
  const [overlapExpiresAt, setOverlapExpiresAt] = useState<number | null>(null);
  const [actionErr, setActionErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Editable settings inside detail drawer
  const [editTargetKind, setEditTargetKind] = useState<AgentEndpointTargetKind>("inbox");
  const [editRoutineId, setEditRoutineId] = useState("");
  const [editAllowlist, setEditAllowlist] = useState("");
  const [editRateLimit, setEditRateLimit] = useState("60");

  const copyText = useCallback(
    (value: string, msg: string) => {
      const done = () => c.toast("ok", msg);
      if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
        void navigator.clipboard
          .writeText(value)
          .then(done)
          .catch(() => c.toast("err", t("Could not copy to clipboard.")));
        return;
      }
      c.toast("err", t("Clipboard access is unavailable."));
    },
    [c, t],
  );

  const load = useCallback(() => {
    if (!c.operator || isMaster) return;
    setLoadErr(null);
    endpointsApi
      .list(c.alias)
      .then((res) => {
        setNotServed(false);
        const list = res.endpoints ?? [];
        setEndpoints(list);
        onEndpointsChange?.(list);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && (e.status === 501 || e.status === 404)) {
          setNotServed(true);
          setEndpoints([]);
          onEndpointsChange?.([]);
          return;
        }
        setLoadErr(e instanceof ApiError ? e.message : t("Could not load incoming endpoints."));
      });
  }, [c.alias, c.operator, isMaster, onEndpointsChange, t]);

  useEffect(() => {
    load();
  }, [load]);

  const syncEditForm = useCallback((ep: AgentEndpoint) => {
    setEditTargetKind(ep.target_kind);
    setEditRoutineId(ep.routine_id ?? "");
    setEditAllowlist((ep.sender_allowlist ?? []).join(", "));
    setEditRateLimit(String(ep.rate_limit_rpm || 60));
  }, []);

  const refreshDetail = useCallback(
    async (id: string) => {
      try {
        const res = await endpointsApi.show(id);
        setDetail(res.endpoint);
        setDeliveries(res.deliveries ?? []);
        syncEditForm(res.endpoint);
      } catch (e: unknown) {
        const fallback = endpoints?.find((item) => item.id === id) ?? null;
        if (fallback) {
          setDetail(fallback);
          syncEditForm(fallback);
        } else {
          setActionErr(e instanceof ApiError ? e.message : t("Could not load endpoint."));
        }
      }
    },
    [endpoints, syncEditForm, t],
  );

  useEffect(() => {
    if (!openId || !c.operator || isMaster) {
      setDetail(null);
      setDeliveries([]);
      return;
    }
    setActionErr(null);
    void refreshDetail(openId);
  }, [openId, c.operator, isMaster, refreshDetail]);

  if (!c.operator) return null;

  if (isMaster) {
    const { company, domain } = companyEmailHost();
    const companyInbox = `${company}@${domain}`;
    return (
      <section className="rtn-ep-section" data-work-panel="master-company-inbox">
        <div className="ap-panel-h">
          <h3>{t("Company inbox (pull-only)")}</h3>
        </div>
        <div className="rtn-ep-notice" role="note">
          {t(
            "Pull-only company inbox (mail_list / mail_read). External emails and webhooks never push-wake Master's private Home thread.",
          )}
        </div>
        <div className="ap-rows">
          <div className="rtn-ep-row-static">
            <span className="rtn-row-main">
              <b>{t("Company email")}</b>
              <span className="rtn-row-path num">{companyInbox}</span>
              <small>{t("Stored in R2; inspect via mail_list / mail_read only")}</small>
            </span>
            <span className="rtn-ep-actions-inline">
              <Button
                size="sm"
                variant="secondary"
                onClick={() => copyText(companyInbox, t("Copied company email."))}
              >
                {t("Copy")}
              </Button>
            </span>
          </div>
        </div>
      </section>
    );
  }

  const resetCreateForm = () => {
    setNewSlug("");
    setNewAuthMode("hmac_sha256");
    setNewRateLimit("60");
    setNewTargetKind("inbox");
    setNewRoutineId(eventRoutines[0]?.id ?? "");
    setNewAllowlist("");
    setCreateErr(null);
  };

  const openCreate = (kind: "email" | "webhook") => {
    resetCreateForm();
    setCreateKind(kind);
  };

  const submitCreate = async () => {
    if (busy || !createKind) return;
    const slug = newSlug.trim().toLowerCase();
    if (!validEndpointSlug(slug)) {
      setCreateErr(t("Slug must be 1–20 lowercase letters, digits, or hyphens."));
      return;
    }
    if (newTargetKind === "routine" && !newRoutineId.trim()) {
      setCreateErr(t("Choose an event routine or switch target to Isolated inbox."));
      return;
    }
    const body: Record<string, unknown> = {
      kind: createKind,
      agent_uid: c.alias,
      slug,
      target_kind: newTargetKind,
    };
    if (newTargetKind === "routine") {
      body.routine_id = newRoutineId.trim();
    }
    if (createKind === "email") {
      const allowlist = parseSenderAllowlist(newAllowlist);
      if (allowlist.length > 0) body.sender_allowlist = allowlist;
    } else {
      const rpm = Number.parseInt(newRateLimit, 10);
      if (!Number.isSafeInteger(rpm) || rpm < 1 || rpm > 600) {
        setCreateErr(t("Rate limit must be an integer between 1 and 600 req/min."));
        return;
      }
      body.auth_mode = newAuthMode;
      body.rate_limit_rpm = rpm;
    }

    setBusy(true);
    setCreateErr(null);
    try {
      const res = await endpointsApi.create(body);
      setCreateKind(null);
      resetCreateForm();
      setRevealedSecret(res.secret ?? null);
      setOverlapExpiresAt(null);
      setDetail(res.endpoint);
      syncEditForm(res.endpoint);
      setDeliveries([]);
      setOpenId(res.endpoint.id);
      load();
      c.toast(
        "ok",
        res.secret
          ? t("Webhook created — copy its one-time secret and enable when ready.")
          : t("Email endpoint created disabled — verify and enable when ready."),
      );
    } catch (e: unknown) {
      setCreateErr(e instanceof ApiError ? e.message : t("Could not create endpoint."));
    } finally {
      setBusy(false);
    }
  };

  const runMutation = async (
    op: () => Promise<{ endpoint: AgentEndpoint }>,
    successMsg: string,
  ) => {
    if (busy || !detail) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res = await op();
      setDetail(res.endpoint);
      syncEditForm(res.endpoint);
      load();
      c.toast("ok", successMsg);
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not update endpoint."));
    } finally {
      setBusy(false);
    }
  };

  const verifyAndEnable = async (ep: AgentEndpoint) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      await endpointsApi.verify(ep.id);
      const res = await endpointsApi.enable(ep.id);
      setDetail(res.endpoint);
      syncEditForm(res.endpoint);
      load();
      c.toast("ok", t("Endpoint verified and enabled."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Verification failed."));
      await refreshDetail(ep.id);
    } finally {
      setBusy(false);
    }
  };

  const rotateWebhookSecret = async (ep: AgentEndpoint) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res = await endpointsApi.rotateSecret(ep.id);
      setDetail(res.endpoint);
      syncEditForm(res.endpoint);
      setRevealedSecret(res.secret);
      setOverlapExpiresAt(res.previous_secret_expires_at ?? res.endpoint.prev_secret_expires_at);
      load();
      c.toast("ok", t("Secret rotated — copy the new secret now."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not rotate secret."));
    } finally {
      setBusy(false);
    }
  };

  const saveSettings = async (ep: AgentEndpoint) => {
    if (busy) return;
    if (editTargetKind === "routine" && !editRoutineId.trim()) {
      setActionErr(t("Choose an event routine or switch target to Isolated inbox."));
      return;
    }
    const patch: Record<string, unknown> = {
      target_kind: editTargetKind,
      routine_id: editTargetKind === "routine" ? editRoutineId.trim() : null,
    };
    if (ep.kind === "email") {
      patch.sender_allowlist = parseSenderAllowlist(editAllowlist);
    } else {
      const rpm = Number.parseInt(editRateLimit, 10);
      if (!Number.isSafeInteger(rpm) || rpm < 1 || rpm > 600) {
        setActionErr(t("Rate limit must be an integer between 1 and 600 req/min."));
        return;
      }
      patch.rate_limit_rpm = rpm;
    }
    await runMutation(() => endpointsApi.update(ep.id, patch), t("Endpoint settings saved."));
  };

  const deleteEndpoint = async (ep: AgentEndpoint) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      await endpointsApi.remove(ep.id);
      setOpenId(null);
      setDetail(null);
      setRevealedSecret(null);
      setOverlapExpiresAt(null);
      load();
      c.toast("ok", t("Endpoint deleted."));
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not delete endpoint."));
    } finally {
      setBusy(false);
    }
  };

  const refreshDeliveries = async (ep: AgentEndpoint) => {
    if (busy) return;
    setBusy(true);
    setActionErr(null);
    try {
      const res = await endpointsApi.deliveries(ep.id, 50);
      setDeliveries(res.deliveries ?? []);
    } catch (e: unknown) {
      setActionErr(e instanceof ApiError ? e.message : t("Could not refresh deliveries."));
    } finally {
      setBusy(false);
    }
  };

  const activeEndpoint = detail ?? endpoints?.find((ep) => ep.id === openId) ?? null;

  const detailMenu: DrawerMenuItem[] =
    activeEndpoint && c.writable
      ? [
          ...(activeEndpoint.state !== "disabled"
            ? [
                {
                  key: "disable",
                  label: t("Disable endpoint"),
                  disabled: busy,
                  onSelect: () => {
                    void runMutation(
                      () => endpointsApi.disable(activeEndpoint.id),
                      t("Endpoint disabled."),
                    );
                  },
                },
              ]
            : []),
          {
            key: "delete",
            label: t("Delete endpoint"),
            destructive: true,
            disabled: busy,
            onSelect: () => {
              void deleteEndpoint(activeEndpoint);
            },
          },
        ]
      : [];

  return (
    <section className="rtn-ep-section" data-work-panel="incoming-endpoints">
      <div className="ap-panel-h">
        <div className="flex items-center gap-2 min-w-0">
          <h3>{t("Incoming endpoints")}</h3>
          {endpoints !== null && !notServed && (
            <span className="text-micro text-ink-500 num">{endpoints.length}</span>
          )}
        </div>
        {c.writable && !notServed && (
          <div className="rtn-ep-head-actions">
            <Button size="sm" variant="secondary" onClick={() => openCreate("email")}>
              {t("+ Email")}
            </Button>
            <Button size="sm" variant="secondary" onClick={() => openCreate("webhook")}>
              {t("+ Webhook")}
            </Button>
          </div>
        )}
      </div>

      {notServed ? (
        <p className="text-label text-ink-500">
          {t("This deployment does not expose workspace incoming endpoints yet.")}
        </p>
      ) : (
        <>
          <div className="rtn-ep-notice" role="note">
            {t(
              "Incoming emails and webhooks are treated as untrusted external data. Read-only lookups run automatically; writes stage for your approval in To-do.",
            )}
          </div>

          {loadErr && (
            <p className="text-label text-fail" role="alert">
              {loadErr}{" "}
              <button type="button" className="lnk" onClick={load}>
                {t("Retry")}
              </button>
            </p>
          )}

          {endpoints === null && !loadErr ? (
            <p className="text-label text-ink-500">{t("Loading incoming endpoints…")}</p>
          ) : endpoints !== null && endpoints.length === 0 ? (
            <p className="text-label text-ink-500">
              {t(
                "No incoming email or webhook endpoints yet. Add one to route external messages into an isolated inbox or event routine.",
              )}
            </p>
          ) : (
            <div className="ap-rows">
              {(endpoints ?? []).map((ep) => {
                const status = endpointStatusMeta(ep);
                const addr = endpointAddress(ep);
                const boundRoutine = ep.routine_id
                  ? eventRoutines.find((r) => r.id === ep.routine_id)?.name ?? ep.routine_id
                  : null;
                return (
                  <div key={ep.id} className="rtn-ep-item" data-endpoint-id={ep.id}>
                    <button
                      type="button"
                      className="rtn-row"
                      onClick={() => {
                        setRevealedSecret(null);
                        setOverlapExpiresAt(null);
                        setOpenId(ep.id);
                      }}
                    >
                      <span className="rtn-row-main">
                        <b>
                          {ep.kind === "email"
                            ? t("Email · {slug}").replace("{slug}", ep.slug)
                            : ep.auth_mode === "hmac_sha256"
                              ? t("Webhook · HMAC-SHA256 · {slug}").replace("{slug}", ep.slug)
                              : t("Webhook · Bearer · {slug}").replace("{slug}", ep.slug)}
                        </b>
                        <span className="rtn-row-path num">{addr}</span>
                        <small>
                          {boundRoutine
                            ? t("Routine: {name}").replace("{name}", boundRoutine)
                            : t("Isolated agent inbox")}
                          {ep.kind === "webhook"
                            ? ` · ${ep.rate_limit_rpm} req/min`
                            : ep.sender_allowlist.length > 0
                              ? ` · ${ep.sender_allowlist.length} allowed sender(s)`
                              : ""}
                        </small>
                      </span>
                      <span className="rtn-ep-row-right">
                        <span className="rtn-pill" data-tone={status.tone}>
                          <span className="rtn-pill-dot" aria-hidden />
                          {t(status.label)}
                        </span>
                      </span>
                    </button>
                    <div className="rtn-ep-item-copy">
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={(e) => {
                          e.stopPropagation();
                          copyText(
                            addr,
                            ep.kind === "email"
                              ? t("Copied endpoint email.")
                              : t("Copied webhook URL."),
                          );
                        }}
                      >
                        {t("Copy")}
                      </Button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </>
      )}

      {createKind && (
        <DrawerShell
          kind={createKind === "email" ? "endpoint-email-new" : "endpoint-webhook-new"}
          scrim="board"
          modal
          trapFocus
          size="md"
          preventClose={busy}
          label={createKind === "email" ? t("New email endpoint") : t("New webhook endpoint")}
          title={createKind === "email" ? t("New email endpoint") : t("New webhook endpoint")}
          subtitle={`${t("Agent")}: ${c.alias} · ${t("Starts disabled")}`}
          onClose={() => setCreateKind(null)}
          secondary={
            <Button size="sm" variant="ghost" onClick={() => setCreateKind(null)} disabled={busy}>
              {t("Cancel")}
            </Button>
          }
          primary={
            <Button size="sm" variant="primary" onClick={() => void submitCreate()} loading={busy}>
              {createKind === "email" ? t("Create email endpoint") : t("Create webhook endpoint")}
            </Button>
          }
        >
          <div className="rtn-drawer-form">
            <div className="rtn-field">
              <label htmlFor="ep-slug">{t("Slug (1–20 lowercase letters, digits, hyphens)")}</label>
              <input
                id="ep-slug"
                className="rtn-input num"
                value={newSlug}
                onChange={(e) => setNewSlug(e.target.value)}
                placeholder={createKind === "email" ? "leads" : "shopify-orders"}
              />
              {createKind === "email" && (
                <span className="text-micro text-ink-400 num break-all">
                  {t("Address preview:")} {formatEndpointEmail(newSlug)}
                </span>
              )}
            </div>

            {createKind === "webhook" && (
              <>
                <div className="rtn-field">
                  <span className="rtn-field-label">{t("Authentication profile")}</span>
                  <div className="rtn-seg" role="radiogroup" aria-label={t("Authentication profile")}>
                    <button
                      type="button"
                      role="radio"
                      aria-checked={newAuthMode === "hmac_sha256"}
                      className="rtn-seg-btn"
                      onClick={() => setNewAuthMode("hmac_sha256")}
                    >
                      <b>{t("HMAC-SHA256")}</b>
                      <small>{t("X-Signature-256 + ±5m X-Timestamp")}</small>
                    </button>
                    <button
                      type="button"
                      role="radio"
                      aria-checked={newAuthMode === "bearer"}
                      className="rtn-seg-btn"
                      onClick={() => setNewAuthMode("bearer")}
                    >
                      <b>{t("Bearer token")}</b>
                      <small>{t("Authorization: Bearer + Idempotency-Key")}</small>
                    </button>
                  </div>
                </div>
                <div className="rtn-field">
                  <label htmlFor="ep-rpm">{t("Rate limit (requests per minute, 1–600)")}</label>
                  <input
                    id="ep-rpm"
                    type="number"
                    min={1}
                    max={600}
                    className="rtn-input num"
                    value={newRateLimit}
                    onChange={(e) => setNewRateLimit(e.target.value)}
                  />
                </div>
              </>
            )}

            <div className="rtn-field">
              <span className="rtn-field-label">{t("Delivery target")}</span>
              <div className="rtn-seg" role="radiogroup" aria-label={t("Delivery target")}>
                <button
                  type="button"
                  role="radio"
                  aria-checked={newTargetKind === "inbox"}
                  className="rtn-seg-btn"
                  onClick={() => setNewTargetKind("inbox")}
                >
                  <b>{t("Isolated inbox")}</b>
                  <small>{t("Dedicated confined session for this endpoint")}</small>
                </button>
                <button
                  type="button"
                  role="radio"
                  aria-checked={newTargetKind === "routine"}
                  className="rtn-seg-btn"
                  onClick={() => setNewTargetKind("routine")}
                >
                  <b>{t("Event routine")}</b>
                  <small>{t("Trigger an approved Playbook or script")}</small>
                </button>
              </div>
            </div>

            {newTargetKind === "routine" && (
              <div className="rtn-field">
                <label htmlFor="ep-new-routine">{t("Event routine")}</label>
                <select
                  id="ep-new-routine"
                  className="rtn-input"
                  value={newRoutineId}
                  onChange={(e) => setNewRoutineId(e.target.value)}
                >
                  <option value="">{t("Choose an event routine…")}</option>
                  {eventRoutines.map((r) => (
                    <option key={r.id} value={r.id}>
                      {r.name} ({r.status})
                    </option>
                  ))}
                </select>
              </div>
            )}

            {createKind === "email" && (
              <div className="rtn-field">
                <label htmlFor="ep-allowlist">
                  {t("Sender allowlist (optional, comma or newline separated)")}
                </label>
                <textarea
                  id="ep-allowlist"
                  rows={2}
                  className="rtn-input num"
                  value={newAllowlist}
                  onChange={(e) => setNewAllowlist(e.target.value)}
                  placeholder="billing@supplier.com, *@partner.com"
                />
              </div>
            )}

            {createErr && (
              <p className="text-label text-fail" role="alert">
                {createErr}
              </p>
            )}
          </div>
        </DrawerShell>
      )}

      {activeEndpoint && (
        <DrawerShell
          key={activeEndpoint.id}
          kind="endpoint"
          scrim="board"
          modal
          trapFocus
          size="md"
          preventClose={busy}
          label={endpointAddress(activeEndpoint)}
          title={
            activeEndpoint.kind === "email"
              ? t("Email · {slug}").replace("{slug}", activeEndpoint.slug)
              : t("Webhook · {slug}").replace("{slug}", activeEndpoint.slug)
          }
          subtitle={endpointAddress(activeEndpoint)}
          pills={
            <span className="rtn-pill" data-tone={endpointStatusMeta(activeEndpoint).tone}>
              <span className="rtn-pill-dot" aria-hidden />
              {t(endpointStatusMeta(activeEndpoint).label)}
            </span>
          }
          warning={
            !activeEndpoint.verified
              ? t("Endpoint starts disabled and unverified. External deliveries are rejected at edge until verified and enabled.")
              : activeEndpoint.state === "paused"
                ? t("Paused: valid deliveries are held in bounded backlog (up to {cap} items) until re-enabled.").replace(
                    "{cap}",
                    String(activeEndpoint.hold_capacity),
                  )
                : undefined
          }
          onClose={() => {
            setOpenId(null);
            setDetail(null);
            setRevealedSecret(null);
            setOverlapExpiresAt(null);
          }}
          menu={detailMenu}
          secondary={
            c.writable
              ? !activeEndpoint.verified
                ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() =>
                        void runMutation(
                          () => endpointsApi.verify(activeEndpoint.id),
                          t("Endpoint verified (still disabled until enabled)."),
                        )
                      }
                      disabled={busy}
                    >
                      {t("Verify only")}
                    </Button>
                  )
                : activeEndpoint.state === "enabled"
                  ? (
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() =>
                          void runMutation(
                            () => endpointsApi.pause(activeEndpoint.id),
                            t("Endpoint paused."),
                          )
                        }
                        disabled={busy}
                      >
                        {t("Pause")}
                      </Button>
                    )
                  : undefined
              : undefined
          }
          primary={
            c.writable
              ? !activeEndpoint.verified
                ? (
                    <Button
                      size="sm"
                      variant="primary"
                      onClick={() => void verifyAndEnable(activeEndpoint)}
                      loading={busy}
                    >
                      {t("Verify & enable")}
                    </Button>
                  )
                : activeEndpoint.state !== "enabled"
                  ? (
                      <Button
                        size="sm"
                        variant="primary"
                        onClick={() =>
                          void runMutation(
                            () => endpointsApi.enable(activeEndpoint.id),
                            t("Endpoint enabled."),
                          )
                        }
                        loading={busy}
                      >
                        {activeEndpoint.state === "paused" ? t("Resume") : t("Enable")}
                      </Button>
                    )
                  : undefined
              : undefined
          }
          tabs={[
            {
              id: "overview",
              label: t("Overview"),
              panel: (
                <div className="rtn-drawer-form">
                  {actionErr && (
                    <p className="text-label text-fail" role="alert">
                      {actionErr}
                    </p>
                  )}

                  {revealedSecret && (
                    <div className="rtn-ep-secret-box" data-secret-reveal>
                      <div className="flex items-center justify-between gap-2">
                        <span className="slabel text-warn">{t("One-time secret — shown once")}</span>
                        <Button
                          size="sm"
                          variant="primary"
                          onClick={() => copyText(revealedSecret, t("Copied secret."))}
                        >
                          {t("Copy secret")}
                        </Button>
                      </div>
                      <code className="rtn-ep-code num">{revealedSecret}</code>
                      <p className="text-micro text-ink-400">
                        {overlapExpiresAt
                          ? t("Prior secret remains accepted until {at} (15-minute overlap window). Closing clears this reveal.").replace(
                              "{at}",
                              fmtInZone(overlapExpiresAt, viewerZone) ?? fmtTime(overlapExpiresAt),
                            )
                          : t("Store this secret now — it cannot be read again once you close this drawer.")}
                      </p>
                    </div>
                  )}

                  <div className="rtn-card-box">
                    <div className="flex items-center justify-between gap-2">
                      <span className="slabel">
                        {activeEndpoint.kind === "email" ? t("Email address") : t("Webhook URL")}
                      </span>
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() =>
                          copyText(
                            endpointAddress(activeEndpoint),
                            activeEndpoint.kind === "email"
                              ? t("Copied endpoint email.")
                              : t("Copied webhook URL."),
                          )
                        }
                      >
                        {t("Copy")}
                      </Button>
                    </div>
                    <code className="rtn-ep-code num">{endpointAddress(activeEndpoint)}</code>
                    <dl className="space-y-1">
                      <div className="rtn-kv">
                        <dt>{t("Revision")}</dt>
                        <dd className="num">rev {activeEndpoint.config_revision ?? "—"}</dd>
                      </div>
                      <div className="rtn-kv">
                        <dt>{t("Hold capacity")}</dt>
                        <dd className="num">{activeEndpoint.hold_capacity}</dd>
                      </div>
                      {activeEndpoint.kind === "webhook" && (
                        <>
                          <div className="rtn-kv">
                            <dt>{t("Auth profile")}</dt>
                            <dd className="num">{activeEndpoint.auth_mode}</dd>
                          </div>
                          <div className="rtn-kv">
                            <dt>{t("Secret hint")}</dt>
                            <dd className="num">{activeEndpoint.secret_hint ?? t("Hidden")}</dd>
                          </div>
                          {activeEndpoint.prev_secret_expires_at &&
                            activeEndpoint.prev_secret_expires_at > Date.now() && (
                              <div className="rtn-kv">
                                <dt>{t("Overlap expires")}</dt>
                                <dd className="num">
                                  {fmtInZone(activeEndpoint.prev_secret_expires_at, viewerZone) ??
                                    fmtTime(activeEndpoint.prev_secret_expires_at)}
                                </dd>
                              </div>
                            )}
                        </>
                      )}
                    </dl>
                  </div>

                  {activeEndpoint.kind === "webhook" && (
                    <div className="rtn-card-box">
                      <div className="flex items-center justify-between gap-2">
                        <span className="slabel">{t("Credentials & curl test")}</span>
                        <div className="flex flex-wrap gap-1.5">
                          <Button
                            size="sm"
                            variant="secondary"
                            onClick={() =>
                              copyText(
                                buildWebhookCurlSnippet(activeEndpoint, revealedSecret),
                                t("Copied curl snippet."),
                              )
                            }
                          >
                            {t("Copy curl")}
                          </Button>
                          {c.writable && (
                            <Button
                              size="sm"
                              variant="secondary"
                              onClick={() => void rotateWebhookSecret(activeEndpoint)}
                              disabled={busy}
                            >
                              {t("Rotate secret")}
                            </Button>
                          )}
                        </div>
                      </div>
                      <pre className="rtn-ep-code num">
                        {buildWebhookCurlSnippet(activeEndpoint, revealedSecret)}
                      </pre>
                    </div>
                  )}

                  {c.writable && (
                    <div className="rtn-card-box">
                      <span className="slabel">{t("Routing & limits")}</span>
                      <div className="rtn-field">
                        <label htmlFor="ep-edit-target">{t("Delivery target")}</label>
                        <select
                          id="ep-edit-target"
                          className="rtn-input"
                          value={editTargetKind}
                          onChange={(e) =>
                            setEditTargetKind(e.target.value === "routine" ? "routine" : "inbox")
                          }
                        >
                          <option value="inbox">{t("Isolated inbox")}</option>
                          <option value="routine">{t("Event routine")}</option>
                        </select>
                      </div>
                      {editTargetKind === "routine" && (
                        <div className="rtn-field">
                          <label htmlFor="ep-edit-routine">{t("Event routine")}</label>
                          <select
                            id="ep-edit-routine"
                            className="rtn-input"
                            value={editRoutineId}
                            onChange={(e) => setEditRoutineId(e.target.value)}
                          >
                            <option value="">{t("Choose an event routine…")}</option>
                            {editRoutineId &&
                              !eventRoutines.some((r) => r.id === editRoutineId) && (
                                <option value={editRoutineId}>{editRoutineId}</option>
                              )}
                            {eventRoutines.map((r) => (
                              <option key={r.id} value={r.id}>
                                {r.name} ({r.status})
                              </option>
                            ))}
                          </select>
                        </div>
                      )}
                      {activeEndpoint.kind === "email" ? (
                        <div className="rtn-field">
                          <label htmlFor="ep-edit-allowlist">{t("Sender allowlist")}</label>
                          <textarea
                            id="ep-edit-allowlist"
                            rows={2}
                            className="rtn-input num"
                            value={editAllowlist}
                            onChange={(e) => setEditAllowlist(e.target.value)}
                            placeholder="billing@supplier.com, *@partner.com"
                          />
                        </div>
                      ) : (
                        <div className="rtn-field">
                          <label htmlFor="ep-edit-rpm">{t("Rate limit (req/min, 1–600)")}</label>
                          <input
                            id="ep-edit-rpm"
                            type="number"
                            min={1}
                            max={600}
                            className="rtn-input num"
                            value={editRateLimit}
                            onChange={(e) => setEditRateLimit(e.target.value)}
                          />
                        </div>
                      )}
                      <div className="flex justify-end">
                        <Button
                          size="sm"
                          variant="secondary"
                          onClick={() => void saveSettings(activeEndpoint)}
                          disabled={busy}
                        >
                          {t("Save settings")}
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              ),
            },
            {
              id: "deliveries",
              label: t("Recent deliveries"),
              badge: deliveries.length > 0 ? <span className="num">{deliveries.length}</span> : undefined,
              panel: (
                <div className="rtn-drawer-form">
                  <div className="flex justify-end">
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => void refreshDeliveries(activeEndpoint)}
                      disabled={busy}
                    >
                      {t("Refresh")}
                    </Button>
                  </div>
                  {deliveries.length === 0 ? (
                    <p className="text-label text-ink-500">{t("No deliveries recorded yet.")}</p>
                  ) : (
                    <div className="ap-rows">
                      {deliveries.map((d) => (
                        <div key={d.id} className="ap-row flex-wrap gap-2">
                          <span className="min-w-0 flex-1">
                            <b className="text-ink-200">{d.subject || d.event_id}</b>
                            <small className="block text-micro text-ink-400">
                              {d.sender}
                              {d.refusal_code ? ` · ${d.refusal_code}` : ""}
                              {d.auth_withheld ? ` · ${t("DKIM/DMARC fail — body withheld")}` : ""}
                            </small>
                          </span>
                          <span className="flex items-center gap-2">
                            <span className="rtn-pill" data-tone={deliveryOutcomeTone(d.outcome)}>
                              <span className="rtn-pill-dot" aria-hidden />
                              {d.outcome}
                            </span>
                            <small className="num">
                              {d.created_at != null
                                ? (fmtInZone(d.created_at, viewerZone) ?? fmtTime(d.created_at))
                                : "—"}
                            </small>
                          </span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ),
            },
          ]}
        />
      )}
    </section>
  );
}
