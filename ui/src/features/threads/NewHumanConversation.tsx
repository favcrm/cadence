import { useId, useRef, useState } from "react";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import { IconClose } from "../../ui/icons";
import { newMessageId } from "../home/thread";
import HumanMemberPicker from "./HumanMemberPicker";
import { createHumanGroup, openHumanDm, type HumanAgent, type HumanMe, type HumanPerson } from "./humanThreadsClient";

export default function NewHumanConversation({ mode, me, agents, agentsError, onCreated, onCancel, onBusy }: {
  mode: "direct" | "group";
  onBusy: (busy: boolean) => void;
  me: HumanMe;
  agents: HumanAgent[] | null;
  agentsError: string | null;
  onCreated: (id: string) => void;
  onCancel: () => void;
}) {
  const id = useId();
  const [selected, setSelected] = useState<HumanPerson[]>([]);
  const [selectedAgents, setSelectedAgents] = useState<HumanAgent[]>([]);
  const [title, setTitle] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const envelope = useRef<{ key: string; id: string } | null>(null);
  const group = mode === "group";
  const count = selected.length + selectedAgents.length;
  return (
    <form className="threads-create-form" aria-label={group ? "New group" : "New direct message"}
      onSubmit={(event) => {
        event.preventDefault();
        if (pending || count === 0) return;
        setPending(true);
        onBusy(true);
        setError(null);
        const submit = async () => {
          if (!group) return openHumanDm(selected[0].subject);
          const input = {
            kind: selectedAgents.length > 0 ? "mixed_group" as const : "human_group" as const,
            title: title.trim() || "Group",
            humans: selected.map((person) => person.subject),
            agents: selectedAgents.map((agent) => agent.uid),
          };
          const key = JSON.stringify(input);
          if (envelope.current?.key !== key) envelope.current = { key, id: newMessageId() };
          return createHumanGroup({ ...input, client_id: envelope.current.id });
        };
        void submit().then((room) => { envelope.current = null; onCreated(room); })
          .catch((failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)))
          .finally(() => { setPending(false); onBusy(false); });
      }}>
      <div className="threads-create-body">
        <p className="threads-note">{group ? "Bring people together around a topic. Add an agent when you need one." : "A private conversation with one person in your workspace."}</p>
        {group && <label className="threads-form-field" htmlFor={`${id}-title`}>
          <span className="threads-field-label">Group name <span className="threads-optional">optional</span></span>
          <input className="field" id={`${id}-title`} value={title} disabled={pending} maxLength={200}
            placeholder="e.g. Design review" onChange={(event) => setTitle(event.target.value)} />
        </label>}
        <HumanMemberPicker actorId={me.id} selected={selected} onChange={setSelected} multiple={group} disabled={pending} />
        {group && <div className="threads-form-field">
          <label className="threads-field-label" htmlFor={`${id}-agent`}>Add an agent <span className="threads-optional">optional</span></label>
          {selectedAgents.length > 0 && <div className="threads-picked" aria-label="Selected agents">{selectedAgents.map((agent) => <span className="threads-picked-person" key={agent.uid}><span>{agent.alias}</span><Button variant="ghost" size="sm" disabled={pending} icon={<IconClose size={12} />} aria-label={`Remove agent ${agent.alias}`} onClick={() => setSelectedAgents((current) => current.filter((pick) => pick.uid !== agent.uid))} /></span>)}</div>}
          <Select id={`${id}-agent`} full searchable value="" disabled={pending || agents === null}
            placeholder={agents === null ? agentsError ? "Agents unavailable" : "Loading agents…" : agents.length === 0 ? "No agents available" : "Choose an agent"}
            options={(agents ?? []).filter((agent) => !selectedAgents.some((pick) => pick.uid === agent.uid)).map((agent) => ({ value: agent.uid, label: agent.alias }))}
            onChange={(uid) => { const agent = agents?.find((candidate) => candidate.uid === uid); if (agent) setSelectedAgents((current) => [...current, agent]); }} />
          {agentsError && <p className="threads-note" role="status">Agents unavailable: {agentsError}. You can still create a human-only group.</p>}
        </div>}
        <p className="threads-note">Only conversation members can read messages. You are included automatically.</p>
        {error && <p className="threads-create-error" role="alert">{error}</p>}
      </div>
      <div className="threads-create-footer">
        <span className="threads-note" role="status">{count ? `${count} selected` : "Choose a member to continue"}</span>
        <div><Button variant="secondary" type="button" onClick={onCancel} disabled={pending}>Cancel</Button><Button variant="primary" type="submit" disabled={pending || count === 0}>{pending ? "Creating…" : group ? "Create group" : "Start message"}</Button></div>
      </div>
    </form>
  );
}
