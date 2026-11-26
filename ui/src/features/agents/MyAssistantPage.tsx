import { useContext, useEffect, useState } from "react";
import { api } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { PersonalAssistantRow } from "../../lib/types";
import Button from "../../ui/Button";
import type { ToastMsg } from "../../ui/Toast";
import { WriteGate } from "../auth/WriteGate";
import { readPersonalIdentity } from "../assistant/identity";
import AgentAvatar, { agentLook } from "./AgentAvatar";
import { OptInTools, WorkspaceSkills, type TabContext } from "./AgentTabs";
import type { AgentTab } from "./agentTabModel";
import InstructionsEditor from "./InstructionsEditor";
import { contextOf } from "./instructionsDoc";
import { PERSONAL_ALIAS } from "./personalAssistant";
import RoutinesTab from "./RoutinesTab";
import { useInstructionsDoc, type InstructionsDoc } from "./useInstructionsDoc";
import "./agents.css";

const PERSONAL_TABS = [
  { id: "instructions", label: "Instructions" },
  { id: "routines", label: "Routines" },
  { id: "skills", label: "Skills" },
  { id: "tools", label: "Tools" },
] as const;

type PersonalTab = (typeof PERSONAL_TABS)[number]["id"];

function normalizePersonalTab(tab: AgentTab | undefined): PersonalTab {
  if (tab === "routines" || tab === "skills" || tab === "tools" || tab === "instructions") return tab;
  return "instructions";
}

function personalTabContext({
  alias,
  operator,
  writable,
  hostedBoard,
  scopedRoster,
  doc,
  toast,
}: {
  alias: string;
  operator: boolean;
  writable: boolean;
  hostedBoard: boolean;
  scopedRoster: boolean;
  doc: InstructionsDoc;
  toast: (kind: ToastMsg["kind"], text: string) => void;
}): TabContext {
  return {
    alias,
    profile: undefined,
    observed: undefined,
    detail: null,
    role: null,
    hosted: hostedBoard,
    hostedBoard,
    scopedRoster,
    resume: undefined,
    onOpenIssue: () => {},
    hired: undefined,
    operator,
    writable,
    readOnlyReason: null,
    toast,
    stats: [],
    detailFailed: false,
    detailNotServed: false,
    retryDetail: () => {},
    roleRead: "read",
    retryRole: () => {},
    doc,
  };
}

/** CAD-2353: `/agents/assistant`, the caller's own assistant. The roster row
 *  names it; the header and routines key refine name, face and `assistant_id`
 *  from the member's own `/api/member-assistant/state` (never `/api/agents/assistant`).
 *  Its chat is Home. Its tabs align with agent pages: Instructions, Routines
 *  (with Incoming endpoints), Skills, and Tools (owner-gated opt-in groups). */
export default function MyAssistantPage({
  row,
  tab,
  onTab,
  operator = false,
  hostedBoard = true,
  scopedRoster = false,
  onBack,
  onToast,
  rosterStale = false,
}: {
  row: PersonalAssistantRow;
  tab?: AgentTab;
  onTab?: (next: AgentTab) => void;
  operator?: boolean;
  hostedBoard?: boolean;
  scopedRoster?: boolean;
  onBack: () => void;
  onToast: (kind: ToastMsg["kind"], text: string) => void;
  /** The roster shows its last answer because the board isn't answering now. */
  rosterStale?: boolean;
}) {
  const t = useTranslate();
  const boardBlock = useContext(WriteGate);
  const writable = boardBlock === null && !rosterStale;
  const [localTab, setLocalTab] = useState<PersonalTab>("instructions");
  const section = onTab ? normalizePersonalTab(tab) : tab && tab !== "overview" ? normalizePersonalTab(tab) : localTab;
  const selectTab = (next: PersonalTab) => {
    setLocalTab(next);
    onTab?.(next);
  };

  const doc = useInstructionsDoc({
    alias: PERSONAL_ALIAS,
    kind: "personal",
    profileId: null,
    nativeServed: null,
    writable,
    onSaved: () => undefined,
    toast: onToast,
  });
  const context = contextOf(doc.draft ?? doc.served ?? "", doc.kind, doc.applies, doc.builtInInstructionTokens);
  const [own, setOwn] = useState<{ name: string | null; look: string } | null>(null);
  const [assistantId, setAssistantId] = useState<string | null>(null);
  const [assistantRead, setAssistantRead] = useState<"reading" | "ready" | "failed">("reading");
  const [assistantAttempt, setAssistantAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setAssistantRead((cur) => (cur === "ready" ? cur : "reading"));
    api
      .memberAssistState()
      .then((state) => {
        if (!live) return;
        const read = readPersonalIdentity(state);
        if (read.state === "ready") setOwn({ name: read.identity.name, look: read.identity.look });
        const id = state.assistant?.assistant_id;
        if (typeof id === "string" && id.trim().length > 0) {
          setAssistantId(id);
          setAssistantRead("ready");
        } else {
          setAssistantId(null);
          setAssistantRead("failed");
        }
      })
      // The roster's name and face already read; a failed refinement keeps them.
      .catch(() => {
        if (live) setAssistantRead("failed");
      });
    return () => {
      live = false;
    };
  }, [assistantAttempt]);

  const name = own?.name ?? row.name;
  const look = own?.look ?? row.look;
  const routinesCtx = personalTabContext({
    alias: assistantId ?? "",
    operator,
    writable: operator && writable,
    hostedBoard,
    scopedRoster,
    doc,
    toast: onToast,
  });
  const toolsCtx = personalTabContext({
    alias: PERSONAL_ALIAS,
    operator,
    writable,
    hostedBoard,
    scopedRoster,
    doc,
    toast: onToast,
  });

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <button type="button" className="rc-back" onClick={onBack}>
        ← {t("Agents")}
      </button>
      <header className="ap-hero">
        <AgentAvatar slug={agentLook(PERSONAL_ALIAS, { look })} still size={48} />
        <div className="min-w-0">
          <h1>{t("My assistant")}</h1>
          <div className="ap-sub wrap-anywhere">
            <span>
              {name} · {t("Your assistant")}
            </span>
          </div>
        </div>
        <div className="ap-acts">
          <Button href="/?chat=assistant">{t("Open Home")}</Button>
        </div>
      </header>
      <nav className="ap-tabs" role="tablist" aria-label={t("Agent sections")}>
        {PERSONAL_TABS.map((entry) => (
          <button
            key={entry.id}
            type="button"
            role="tab"
            id={`ap-tab-${entry.id}`}
            aria-selected={section === entry.id}
            aria-controls="ap-panel"
            className="ap-tab"
            onClick={() => selectTab(entry.id)}
          >
            {t(entry.label)}
          </button>
        ))}
      </nav>
      <div id="ap-panel" role="tabpanel" aria-labelledby={`ap-tab-${section}`}>
        {section === "routines" ? (
          !operator ? (
            <RoutinesTab c={routinesCtx} />
          ) : assistantRead === "reading" ? (
            <p className="rc-note">{t("Loading routines…")}</p>
          ) : assistantRead === "failed" || !assistantId ? (
            <p className="rc-note" role="alert">
              {t("Your assistant ID could not be loaded.")}{" "}
              <button type="button" className="ap-link" onClick={() => setAssistantAttempt((n) => n + 1)}>
                {t("Retry")}
              </button>
            </p>
          ) : (
            <RoutinesTab key={`routine:${assistantId}`} c={routinesCtx} />
          )
        ) : section === "skills" ? (
          doc.read === "unsupported" ? (
            <p className="rc-note">{t("Your assistant’s skills are not edited here yet.")}</p>
          ) : (
            <div className="ap-body">
              <WorkspaceSkills
                doc={doc}
                writable={writable}
                canManageLibrary={operator && writable}
                agentName={name}
                toast={onToast}
                absent={null}
                scopedRoster={scopedRoster}
              />
            </div>
          )
        ) : section === "tools" ? (
          <div className="ap-body">
            {!operator && doc.read === "ready" && (
              <p className="rc-note">{t("Opt-in tool groups are managed by the workspace owner.")}</p>
            )}
            <OptInTools c={toolsCtx} />
          </div>
        ) : doc.read === "unsupported" ? (
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
