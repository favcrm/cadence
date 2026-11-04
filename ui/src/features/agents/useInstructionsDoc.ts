import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import { api } from "../../lib/api";
import type { StoredAgentProfile } from "../../lib/types";
import type { ToastMsg } from "../../ui/Toast";
import {
  checkHosted,
  checkNative,
  saveHosted,
  saveNative,
  saveOverTheirs,
  savedText,
  type DocDeps,
  type DocKind,
  type DocOutcome,
} from "./instructionsDoc";

const liveDeps: DocDeps = {
  readProfile: async (id) => (await api.agentProfile(id)).profile,
  writeProfile: async (id, body) => (await api.saveAgentProfile(id, body)).profile,
  readNative: async (alias) => (await api.agent(alias)).agent.instructions ?? null,
  writeNative: (alias, text) => api.setAgentInstructions(alias, text),
};

export type DocPhase = "idle" | "saving" | "conflict" | "unconfirmed";

/** Where the caret was when the textarea last lost it, so Preview and a tab
 *  trip give it back. */
export interface DocSelection {
  start: number;
  end: number;
  top: number;
}

/** Everything the Instructions section needs. It lives on the agent page so a
 *  tab trip keeps the draft, the revision it started from and the caret. */
export interface InstructionsDoc {
  /** null: this agent's text is not edited through this document. */
  kind: DocKind | null;
  read: "idle" | "reading" | "failed" | "ready";
  /** The text the board holds, as of the last read or save. */
  served: string | null;
  draft: string | null;
  setDraft: (text: string | null) => void;
  phase: DocPhase;
  /** Hosted only: when and at which revision the stored text last changed. */
  profile: StoredAgentProfile | null;
  selection: MutableRefObject<DocSelection>;
  retry: () => void;
  save: () => void;
  saveMine: () => void;
  useTheirs: () => void;
  check: () => void;
  discard: () => void;
}

export function useInstructionsDoc(opts: {
  alias: string;
  kind: DocKind | null;
  /** Hosted: the profile id. */
  profileId: string | null;
  /** Native: what the agent detail served. */
  nativeServed: string | null;
  writable: boolean;
  onSaved: () => void;
  toast: (kind: ToastMsg["kind"], text: string) => void;
  /** Substituted by tests. */
  deps?: DocDeps;
}): InstructionsDoc {
  const { alias, kind, profileId, nativeServed, writable, onSaved, toast } = opts;
  const deps = opts.deps ?? liveDeps;
  const [draft, setDraftState] = useState<string | null>(null);
  const [phase, setPhase] = useState<DocPhase>("idle");
  const [profile, setProfile] = useState<StoredAgentProfile | null>(null);
  const [read, setRead] = useState<InstructionsDoc["read"]>("idle");
  const [nativeSaved, setNativeSaved] = useState<string | null>(null);
  const [retries, setRetries] = useState(0);
  const selection = useRef<DocSelection>({ start: 0, end: 0, top: 0 });
  const busy = useRef(false);

  useEffect(() => {
    if (kind !== "hosted" || profileId === null) return;
    let live = true;
    setRead("reading");
    deps
      .readProfile(profileId)
      .then((p) => {
        if (!live) return;
        setProfile(p);
        setRead("ready");
      })
      .catch(() => live && setRead("failed"));
    return () => {
      live = false;
    };
  }, [kind, profileId, retries, deps]);

  // A reload that shows our text means the override has done its job.
  useEffect(() => {
    if (nativeSaved !== null && nativeServed === nativeSaved) setNativeSaved(null);
  }, [nativeServed, nativeSaved]);

  const served = kind === "hosted" ? (profile?.responsibilities ?? null) : (nativeSaved ?? nativeServed);
  const state = kind === null ? "idle" : kind === "hosted" ? read : served === null ? "idle" : "ready";

  // A draft equal to the stored text is not a draft. Whitespace the board will
  // trim stays: dropping it would eat a newline the person just typed.
  useEffect(() => {
    if (served !== null && draft === served && phase === "idle") setDraftState(null);
  }, [served, draft, phase]);

  const setDraft = useCallback((text: string | null) => setDraftState(text), []);

  const settle = useCallback(
    (out: DocOutcome, text: string, wasCheck: boolean) => {
      if (kind === null) return;
      switch (out.kind) {
        case "saved":
          if (out.profile) {
            setProfile(out.profile);
            setRead("ready");
          } else setNativeSaved(out.text);
          setDraftState(null);
          setPhase("idle");
          toast("ok", savedText(kind, out.applies, wasCheck));
          if (!out.profile) onSaved();
          return;
        case "conflict":
          if (out.profile) setProfile(out.profile);
          setPhase("conflict");
          toast("warn", "Someone else changed this. Your draft is kept.");
          return;
        case "unconfirmed":
          setPhase("unconfirmed");
          toast("warn", wasCheck ? "Still can’t reach the board. Your draft is kept." : "Not confirmed: the connection dropped. Check before saving again.");
          return;
        case "not_saved":
          if (out.profile) setProfile(out.profile);
          setPhase("idle");
          setDraftState(text);
          toast("warn", "It did not save. Your draft is kept.");
          return;
        case "refused":
          setPhase("idle");
          toast("err", out.message);
      }
    },
    [kind, onSaved, toast],
  );

  const run = useCallback(
    async (job: () => Promise<DocOutcome>, text: string, wasCheck = false) => {
      if (busy.current) return;
      busy.current = true;
      setPhase("saving");
      try {
        settle(await job(), text, wasCheck);
      } finally {
        busy.current = false;
      }
    },
    [settle],
  );

  const text = draft ?? served ?? "";
  const ready = writable && kind !== null && state === "ready";

  return {
    kind,
    read: state,
    served,
    draft,
    setDraft,
    phase,
    profile,
    selection,
    retry: () => setRetries((n) => n + 1),
    save: () => {
      if (!ready || draft === null) return;
      if (kind === "hosted") {
        if (profile) void run(() => saveHosted(deps, profile, draft), draft);
      } else void run(() => saveNative(deps, alias, draft), draft);
    },
    saveMine: () => {
      if (!ready || kind !== "hosted" || profileId === null) return;
      void run(() => saveOverTheirs(deps, profileId, text), text);
    },
    useTheirs: () => {
      if (kind !== "hosted" || profileId === null || busy.current) return;
      deps
        .readProfile(profileId)
        .then((p) => {
          setProfile(p);
          setDraftState(null);
          setPhase("idle");
          toast("ok", "Loaded their version.");
        })
        .catch(() => toast("err", "Could not read their version. Your draft is kept."));
    },
    check: () => {
      if (!ready || draft === null) return;
      if (kind === "hosted") {
        if (profile) void run(() => checkHosted(deps, profile, draft), draft, true);
      } else void run(() => checkNative(deps, alias, draft), draft, true);
    },
    discard: () => {
      setDraftState(null);
      setPhase("idle");
    },
  };
}
