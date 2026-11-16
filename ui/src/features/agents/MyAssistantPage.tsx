import { useContext, useEffect, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { PersonalAssistantRow } from "../../lib/types";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";
import { WriteGate } from "../auth/WriteGate";
import { readPersonalIdentity } from "../assistant/identity";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import InstructionsEditor from "./InstructionsEditor";
import { contextOf } from "./instructionsDoc";
import { PERSONAL_ALIAS } from "./personalAssistant";
import { useInstructionsDoc } from "./useInstructionsDoc";
import "./agents.css";

/** CAD-2353: `/agents/assistant`, the caller's own assistant. The roster row
 *  names it; the header refines name and face from the member's own
 *  `/api/member-assistant/state` (never `/api/agents/assistant`). Its chat is
 *  Home. CAD-2354: its instructions are the member's own to edit, read and
 *  written only for the caller by the board; a board without the route keeps
 *  the read-only note. */
export default function MyAssistantPage({
  row,
  onBack,
  onToast,
  rosterStale = false,
}: {
  row: PersonalAssistantRow;
  onBack: () => void;
  onToast: (kind: ToastMsg["kind"], text: string) => void;
  /** The roster shows its last answer because the board isn't answering now. */
  rosterStale?: boolean;
}) {
  const t = useTranslate();
  const boardBlock = useContext(WriteGate);
  const writable = boardBlock === null && !rosterStale;
  const doc = useInstructionsDoc({
    alias: PERSONAL_ALIAS,
    kind: "personal",
    profileId: null,
    nativeServed: null,
    writable,
    onSaved: () => undefined,
    toast: onToast,
  });
  const context = contextOf(doc.draft ?? doc.served ?? "", doc.kind, doc.applies);
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
        {doc.read === "unsupported" ? (
          <p className="rc-note">{t("Your assistant’s instructions are not edited here yet.")}</p>
        ) : doc.read === "failed" ? (
          <p className="rc-note" role="alert">
            {t("The instructions could not be read.")}{" "}
            <button type="button" className="ap-link" onClick={doc.retry}>
              {t("Retry")}
            </button>
          </p>
        ) : doc.read !== "ready" ? (
          <p className="rc-note">{t("Loading instructions…")}</p>
        ) : (
          <div className="ap-body">
            <InstructionsEditor
              doc={doc}
              writable={writable}
              resetTo={null}
              contextHeading={context.heading}
              contextLines={context.lines}
              contextNote={
                <p className="ap-drawer-lead">
                  {t("Skills in full, wiki pages the agent reads, earlier sessions, and a repository’s own agent instructions file during project work.")}
                </p>
              }
            />
          </div>
        )}
      </div>
    </main>
  );
}
