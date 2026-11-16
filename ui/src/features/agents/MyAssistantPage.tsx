import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { PersonalAssistantRow } from "../../lib/types";
import Button from "../../ui/Button";
import { readPersonalIdentity } from "../assistant/identity";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import { PERSONAL_ALIAS } from "./personalAssistant";
import "./agents.css";

/** CAD-2353: `/agents/assistant`, the caller's own assistant. The roster row
 *  names it; the header refines name and face from the member's own
 *  `/api/member-assistant/state` (never `/api/agents/assistant`). Its chat is
 *  Home. Instructions are read-only until CAD-2354 makes them editable, and
 *  this page shows only what the caller's own assistant already shows them. */
export default function MyAssistantPage({
  row,
  onBack,
}: {
  row: PersonalAssistantRow;
  onBack: () => void;
}) {
  const t = useTranslate();
  const [own, setOwn] = useState<{ name: string | null; look: string } | null>(null);
  useEffect(() => {
    let live = true;
    api
      .memberAssistState()
      .then((state) => {
        const read = readPersonalIdentity(state);
        if (live && read.state === "ready") setOwn({ name: read.identity.name, look: read.identity.look });
      })
      // The roster's name and face already read; a failed refinement keeps them.
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);
  const name = own?.name ?? row.name;
  const look = own?.look ?? row.look;
  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <button type="button" className="rc-back" onClick={onBack}>
        ← {t("Agents")}
      </button>
      <header className="ap-hero">
        <AgentAvatar slug={agentLook(PERSONAL_ALIAS, { look })} still size={48} />
        <div className="min-w-0">
          <h1>{t("My assistant")}</h1>
          <div className="ap-sub">
            <span>
              {name} · {t("Your assistant")}
            </span>
          </div>
        </div>
        <div className="ap-acts">
          <Button href="/">{t("Open Home")}</Button>
        </div>
      </header>
      <nav className="ap-tabs" role="tablist" aria-label={t("Agent sections")}>
        <button type="button" role="tab" id="ap-tab-instructions" aria-selected className="ap-tab" aria-controls="ap-panel">
          {t("Instructions")}
        </button>
      </nav>
      <div id="ap-panel" role="tabpanel" aria-labelledby="ap-tab-instructions">
        <p className="rc-note">{t("Your assistant’s instructions are not edited here yet.")}</p>
      </div>
    </main>
  );
}
