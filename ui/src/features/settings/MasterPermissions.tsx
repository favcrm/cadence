import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { api, ApiError, type MasterPermissionRule } from "../../lib/api";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { IconChevron, IconRefresh, IconSearch } from "../../ui/icons";
import { useWriteBlock } from "../auth/WriteGate";
import "./permissions.css";

type Viewer = {
  operator: boolean | null;
  readOnly: boolean;
  boardReadOnly: boolean;
  signedIn: boolean;
  sessionId: string | null;
};
const errorText = (error: unknown) =>
  error instanceof ApiError ? error.message : String(error);
// Quotes preserve argument boundaries. This is a display, never an executable shell command.
const argument = (value: string) =>
  /^[\w./:@=+*-]+$/.test(value) ? value : JSON.stringify(value);
const command = (rule: MasterPermissionRule) =>
  [...rule.argv, ...(rule.scope === "prefix" ? rule.tail : [])]
    .map(argument)
    .join(" ");
const scopeLabel = (rule: MasterPermissionRule) =>
  rule.scope === "exact" ? "Exact command" : "Argument pattern";
function savedDate(at: number) {
  const date = new Date(at * 1000);
  return Number.isNaN(date.getTime()) ? null : date;
}

function RuleRow({
  rule,
  confirming,
  disabled,
  busy,
  onConfirm,
  onCancel,
  onRevoke,
}: {
  rule: MasterPermissionRule;
  confirming: boolean;
  disabled: boolean;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  onRevoke: () => void;
}) {
  const id = useId();
  const row = useRef<HTMLLIElement>(null);
  const wasConfirming = useRef(false);
  useLayoutEffect(() => {
    if (confirming)
      row.current
        ?.querySelector<HTMLButtonElement>(".permission-confirm .btn")
        ?.focus();
    else if (wasConfirming.current)
      row.current
        ?.querySelector<HTMLButtonElement>(".permission-revoke")
        ?.focus();
    wasConfirming.current = confirming;
  }, [confirming]);
  const date = savedDate(rule.at);
  return (
    <li
      ref={row}
      className="permission-row"
      data-rule={rule.id}
      onKeyDown={(event) => {
        if (event.key === "Escape" && confirming) {
          event.preventDefault();
          onCancel();
        }
      }}
    >
      <div className="permission-row-main">
        <div className="permission-row-labels">
          <span className="permission-effect" data-effect={rule.effect}>
            {rule.effect === "allow" ? "Allow" : "Deny"}
          </span>
          <span>{scopeLabel(rule)}</span>
        </div>
        <code className="permission-command">{command(rule)}</code>
        <p className="permission-folder">
          <span>Working folder</span> <code>{rule.cwd || "Not recorded"}</code>
        </p>
        <div className="permission-provenance">
          <span>Saved by {rule.by || "Not recorded"}</span>
          {date ? (
            <time dateTime={date.toISOString()} title={date.toISOString()}>
              {date.toLocaleDateString(undefined, {
                year: "numeric",
                month: "short",
                day: "numeric",
                timeZone: "UTC",
              })}
            </time>
          ) : (
            <span>Date not recorded</span>
          )}
          <details className="permission-details">
            <summary>
              Rule details <IconChevron />
            </summary>
            <dl>
              <div>
                <dt>Rule ID</dt>
                <dd>
                  <code>{rule.id}</code>
                </dd>
              </div>
              <div>
                <dt>Match</dt>
                <dd>
                  {rule.scope === "exact"
                    ? "Every command argument and the working folder must match exactly."
                    : "The command head matches literally. Each remaining argument must match its saved pattern, with the same argument count and exact working folder. A trailing * matches only at a path boundary."}
                </dd>
              </div>
              <div>
                <dt>Command arguments</dt>
                <dd>
                  <code>{JSON.stringify(rule.argv)}</code>
                </dd>
              </div>
              {rule.scope === "prefix" && (
                <div>
                  <dt>Argument patterns</dt>
                  <dd>
                    <code>{JSON.stringify(rule.tail)}</code>
                  </dd>
                </div>
              )}
              <div>
                <dt>Saved at</dt>
                <dd>{date ? date.toISOString() : "Not recorded"}</dd>
              </div>
            </dl>
          </details>
        </div>
      </div>
      <div className="permission-row-actions">
        {confirming ? (
          <div
            className="permission-confirm"
            aria-describedby={`${id}-confirm`}
          >
            <p id={`${id}-confirm`}>
              Revoke this {rule.effect} rule?{" "}
              {rule.effect === "deny"
                ? "This removes this restriction; an allow rule may then apply."
                : "The master may need permission again."}{" "}
              Takes effect on the next check.
            </p>
            <div>
              <Button onClick={onCancel}>Cancel</Button>
              <Button variant="danger" disabled={disabled} onClick={onConfirm}>
                Confirm revoke
              </Button>
            </div>
          </div>
        ) : (
          <Button
            className="permission-revoke"
            size="sm"
            disabled={disabled}
            loading={busy}
            aria-label={`Revoke ${rule.effect} rule ${rule.id}`}
            onClick={onRevoke}
          >
            {busy ? "Revoking…" : "Revoke"}
          </Button>
        )}
      </div>
    </li>
  );
}

type Flight = {
  busy: string | null;
  begin: (id: string) => boolean;
  finish: () => void;
};
/** Eligible viewers request the server-protected list; access recovery gets a new lifetime. */
function RuleWorkspace({
  readOnly,
  flight,
}: {
  readOnly: boolean;
  flight: Flight;
}) {
  const [rules, setRules] = useState<MasterPermissionRule[] | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [fetching, setFetching] = useState(true);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [effect, setEffect] = useState("");
  const [scope, setScope] = useState("");
  const alive = useRef(false);
  const request = useRef(0);
  const tools = useRef<HTMLDivElement>(null);
  const restoreAfterWrite = useRef(false);
  const filterId = useId();
  const blocked = useWriteBlock(readOnly);
  const load = useCallback(async () => {
    const id = ++request.current;
    setFetching(true);
    setConfirm(null);
    try {
      const next = await api.permissionRules();
      if (!alive.current || request.current !== id) return;
      setRules(next.rules);
      setReadError(null);
    } catch (error) {
      if (alive.current && request.current === id) {
        if (
          error instanceof ApiError &&
          (error.status === 401 || error.status === 403)
        ) {
          // Authorization refusal ends the protected observation; only a
          // transient failure may retain last-known rules.
          setRules(null);
          setNotice(null);
          setActionError(null);
          setConfirm(null);
        }
        setReadError(errorText(error));
      }
    } finally {
      if (alive.current && request.current === id) setFetching(false);
    }
  }, []);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++request.current;
    };
  }, []);
  useEffect(() => {
    // A write invalidates earlier reads. Refresh only after its POST settles,
    // including a write begun before operator access was lost and recovered.
    ++request.current;
    setFetching(true);
    if (!flight.busy) void load();
  }, [flight.busy, load]);
  useEffect(() => {
    setConfirm(null);
  }, [readOnly, query, effect, scope]);
  useLayoutEffect(() => {
    if (flight.busy || !restoreAfterWrite.current || (!notice && !actionError))
      return;
    restoreAfterWrite.current = false;
    tools.current
      ?.querySelector<HTMLButtonElement>(".permission-refresh")
      ?.focus();
  }, [flight.busy, notice, actionError]);
  const disabled =
    !!blocked || !!flight.busy || fetching || !!readError || rules === null;
  const revoke = async (rule: MasterPermissionRule) => {
    if (disabled || confirm !== rule.id || !flight.begin(rule.id)) return;
    restoreAfterWrite.current = true;
    ++request.current;
    setFetching(true);
    setConfirm(null);
    setNotice(null);
    setActionError(null);
    try {
      await api.permissionRevoke(rule.id);
      if (!alive.current) return;
      setRules(
        (previous) => previous?.filter((saved) => saved.id !== rule.id) ?? null,
      );
      setNotice(`Rule revoked: ${rule.id}. Takes effect on the next check.`);
    } catch (error) {
      if (alive.current) setActionError(errorText(error));
    } finally {
      flight.finish();
    }
  };
  const filtered = !!(query.trim() || effect || scope);
  const visible = (rules ?? []).filter(
    (rule) =>
      (!effect || effect === rule.effect) &&
      (!scope || scope === rule.scope) &&
      [rule.id, rule.by, rule.cwd, ...rule.argv, ...rule.tail]
        .join(" ")
        .toLowerCase()
        .includes(query.trim().toLowerCase()),
  );
  const clear = () => {
    setQuery("");
    setEffect("");
    setScope("");
  };
  return (
    <>
      <div className="permissions-tools" ref={tools}>
        <div className="permission-filter permission-search">
          <label htmlFor={`${filterId}-query`}>Search rules</label>
          <div className="permission-search-field">
            <IconSearch size={14} />
            <input
              id={`${filterId}-query`}
              type="search"
              className="field"
              placeholder="Command, folder, ID or author"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </div>
        </div>
        <div className="permission-filter">
          <label htmlFor={`${filterId}-effect`}>Effect</label>
          <Select
            id={`${filterId}-effect`}
            value={effect}
            onChange={setEffect}
            options={[
              { value: "", label: "All effects" },
              { value: "allow", label: "Allow" },
              { value: "deny", label: "Deny" },
            ]}
            full
          />
        </div>
        <div className="permission-filter">
          <label htmlFor={`${filterId}-scope`}>Match</label>
          <Select
            id={`${filterId}-scope`}
            value={scope}
            onChange={setScope}
            options={[
              { value: "", label: "All matches" },
              { value: "exact", label: "Exact command" },
              { value: "prefix", label: "Argument pattern" },
            ]}
            full
          />
        </div>
        <Button
          className="permission-refresh"
          icon={<IconRefresh />}
          disabled={!!flight.busy}
          onClick={() => void load()}
        >
          {readError ? "Retry rules" : "Refresh rules"}
        </Button>
      </div>
      {blocked && (
        <p className="permission-access">
          {blocked} Saved rules remain available to inspect.
        </p>
      )}
      <p className="permission-feedback" role="status">
        {notice}
      </p>
      {actionError && (
        <p className="permission-error" role="alert">
          <strong>Couldn’t revoke the rule.</strong> {actionError}
        </p>
      )}
      {readError && (
        <div className="permission-error" role="alert">
          <strong>
            {rules === null ? "Rules unavailable." : "Last known rules."}
          </strong>{" "}
          {readError}{" "}
          {rules !== null &&
            "Refresh to verify the remaining rules before revoking."}
        </div>
      )}
      <div className="permission-results">
        <span>
          {rules === null
            ? readError
              ? "Rules haven’t been loaded"
              : flight.busy
                ? "Waiting for revocation…"
                : "Loading saved rules…"
            : `${readError ? "Last known · " : ""}${visible.length} of ${rules.length} saved ${rules.length === 1 ? "rule" : "rules"}${fetching ? " · Refreshing…" : ""}`}
        </span>
        {filtered && (
          <Button variant="ghost" size="sm" onClick={clear}>
            Clear filters
          </Button>
        )}
      </div>
      {rules === null ? (
        <div className="permission-state" aria-busy={fetching}>
          <h2>
            {readError
              ? "Couldn’t load saved rules"
              : flight.busy
                ? "Waiting for revocation…"
                : "Loading rules…"}
          </h2>
          <p>
            {readError
              ? "Use Retry rules to try again."
              : flight.busy
                ? "The saved rules will refresh when the current revoke request finishes."
                : "Reading the operator’s saved rules for the master."}
          </p>
        </div>
      ) : visible.length === 0 ? (
        <div className="permission-state">
          <h2>
            {readError
              ? "No last known matches"
              : filtered
                ? "No matching rules"
                : "No saved rules"}
          </h2>
          <p>
            {readError
              ? "Retry to check the current rules."
              : filtered
                ? "Try another search or clear the filters."
                : "Saved allow and deny rules appear here. Respond to live permission requests in Home."}
          </p>
          {filtered && <Button onClick={clear}>Show all rules</Button>}
        </div>
      ) : (
        <ul className="permission-list" role="list" aria-busy={fetching}>
          {visible.map((rule) => (
            <RuleRow
              key={rule.id}
              rule={rule}
              confirming={confirm === rule.id}
              disabled={disabled}
              busy={flight.busy === rule.id}
              onRevoke={() => {
                if (!disabled) {
                  setConfirm(rule.id);
                  setActionError(null);
                }
              }}
              onCancel={() => setConfirm(null)}
              onConfirm={() => void revoke(rule)}
            />
          ))}
        </ul>
      )}
    </>
  );
}

/** Saved rules only. Live permission-request decisions stay in Home. */
export default function MasterPermissions({ viewer }: { viewer: Viewer }) {
  const locked = useRef(false);
  const [busy, setBusy] = useState<string | null>(null);
  const begin = useCallback((id: string) => {
    if (locked.current) return false;
    locked.current = true;
    setBusy(id);
    return true;
  }, []);
  const finish = useCallback(() => {
    locked.current = false;
    setBusy(null);
  }, []);
  return (
    <main className="permissions-workspace" data-settings="permissions">
      <header className="permissions-heading">
        <h1>Master permissions</h1>
        <p>
          Saved command rules for the master. A deny takes precedence over an
          allow; every rule applies within its working folder.
        </p>
      </header>
      {viewer.operator || (viewer.boardReadOnly && viewer.signedIn) ? (
        <RuleWorkspace
          key={JSON.stringify([
            viewer.sessionId,
            viewer.operator ? "operator" : "readonly-session",
          ])}
          readOnly={viewer.readOnly || !viewer.operator}
          flight={{ busy, begin, finish }}
        />
      ) : (
        <div className="permission-state">
          <h2>Operator access required</h2>
          <p>
            Sign in as the operator to inspect and revoke saved rules. Live
            permission requests appear in Home.
          </p>
        </div>
      )}
    </main>
  );
}
