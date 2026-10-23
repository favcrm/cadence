import { useMemo, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import type { Agent, AgentRole } from "../../lib/types";
import { HireGate, GhostCard, RoleCard } from "./RecruitCards";
import { categoryLabel, matchesRole, onTeamCount, type RolesState } from "./agentRoles";

/** The catalogue: category chips with counts, search, grouped cards, then
 *  Ask Master and Start blank. Hire goes through `onPick` (the sheet). */
export default function Recruit({
  roles,
  agents,
  writeBlock,
  onBack,
  onPick,
  onAskMaster,
  onBlank,
}: {
  roles: RolesState;
  agents: Agent[];
  writeBlock: string | null;
  onBack: () => void;
  onPick: (role: AgentRole) => void;
  onAskMaster: () => void;
  onBlank: () => void;
}) {
  const t = useTranslate();
  const [category, setCategory] = useState("all");
  const [query, setQuery] = useState("");
  const payload = roles.data;
  const list = useMemo(
    () => (payload?.roles ?? []).filter((role) => matchesRole(role, category, query)),
    [payload, category, query],
  );
  const groups = category === "all" ? (payload?.categories.map((c) => c.id) ?? []) : [category];
  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <button type="button" className="rc-back" onClick={onBack}>
        ← {t("Agents")}
      </button>
      <div className="rc-head">
        <div>
          <h1>{t("Recruit an agent")}</h1>
          <p>
            {t(
              "Pick a ready-made role. It joins your team, not a project — work comes to it as tickets and chats.",
            )}
          </p>
        </div>
      </div>
      <HireGate reason={writeBlock} />
      {!payload && !roles.error && (
        <p role="status" className="text-label text-ink-500">
          {t("Loading the catalogue…")}
        </p>
      )}
      {roles.error && (
        <div className="space-y-2">
          <p role="alert" className="text-label text-fail">
            {t("Could not load the catalogue:")} {roles.error}
          </p>
          <Button onClick={roles.retry}>{t("Retry")}</Button>
        </div>
      )}
      {payload && (
        <>
          <div className="rc-bar">
            <div className="rc-cats" role="group" aria-label={t("Category")}>
              {[{ id: "all", label: "All" }, ...payload.categories].map((c) => (
                <button
                  key={c.id}
                  type="button"
                  className="rc-cat"
                  aria-pressed={category === c.id}
                  onClick={() => setCategory(c.id)}
                >
                  {t(c.label)}
                  <span className="n">
                    {c.id === "all"
                      ? payload.roles.length
                      : payload.roles.filter((r) => r.category === c.id).length}
                  </span>
                </button>
              ))}
            </div>
            <input
              className="field rc-search"
              type="search"
              aria-label={t("Search roles")}
              placeholder={t("Search roles — e.g. invoice, test, quote")}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </div>
          {groups.map((group) => {
            const rows = list.filter((role) => role.category === group);
            if (rows.length === 0) return null;
            return (
              <section key={group}>
                <div className="rc-grp">{t(categoryLabel(payload, group))}</div>
                <div className="rc-grid">
                  {rows.map((role) => (
                    <RoleCard
                      key={role.id}
                      role={role}
                      payload={payload}
                      onTeam={onTeamCount(role, agents)}
                      onPick={onPick}
                    />
                  ))}
                </div>
              </section>
            );
          })}
          {list.length === 0 && (
            <div className="rc-empty">
              {t("No role matches “{query}”. Ask Master to set one up from your words.").replace(
                "{query}",
                () => query,
              )}
            </div>
          )}
          <div className="rc-grp">{t("Something else")}</div>
          <div className="rc-grid">
            <GhostCard
              look="robot"
              name="master"
              title={t("Ask Master")}
              kicker={t("describe it in words")}
              text={t(
                "“I need someone to chase unpaid invoices every Friday.” Master proposes a role for you to hire.",
              )}
              onClick={onAskMaster}
            />
            <GhostCard
              look="custom"
              name="blank"
              title={t("Start blank")}
              kicker={t("advanced")}
              text={t("Today's form: provider, endpoint, team role, model policy, instructions.")}
              onClick={onBlank}
            />
          </div>
        </>
      )}
    </main>
  );
}
