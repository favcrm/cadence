import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import type { WorkspaceSkillSummary } from "../../lib/types";
import Toast, { type ToastMsg } from "../../ui/Toast";
import { SkillDrawer } from "../agents/SkillDrawer";
import { WorkspaceSkillsDrawer } from "../agents/WorkspaceSkillsDrawer";
import { libraryAbsent, liveSkillDeps } from "../agents/workspaceSkills";
import type { SkillDraftLiveState, SkillDraftReviewSource } from "./TurnRichCards";

const EMPTY_SKILL_MAP: ReadonlyMap<string, SkillDraftLiveState> = new Map();

/**
 * Home-owned workspace-skill draft review state and shared SkillDrawer mount.
 * Keeps cached summaries aligned with live SkillDrawer mutations (approve,
 * edit draft, revoke, delete) and resets cleanly across private session changes.
 */
export function useHomeSkillReview({
  enabled,
  writable,
}: {
  enabled: boolean;
  writable: boolean;
}): {
  source: SkillDraftReviewSource | null;
  drawer: ReactNode;
} {
  const [open, setOpen] = useState<{ name: string; revision?: number } | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [toastMsg, setToastMsg] = useState<ToastMsg | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const toastTimer = useRef<number | null>(null);

  const storeRef = useRef<{
    map: Map<string, SkillDraftLiveState>;
    loaded: boolean;
    loading: boolean;
    listeners: Set<() => void>;
  }>({
    map: new Map(),
    loaded: false,
    loading: false,
    listeners: new Set(),
  });

  const emit = useCallback(() => {
    for (const listener of storeRef.current.listeners) listener();
  }, []);

  const resetStore = useCallback(() => {
    const store = storeRef.current;
    store.loaded = false;
    store.loading = false;
    if (store.map.size > 0) {
      store.map = new Map();
      emit();
    }
    setOpen(null);
    setLibraryOpen(false);
  }, [emit]);

  useEffect(() => {
    if (!enabled) {
      resetStore();
      return;
    }
    const stopPause = onPrivateReadsPaused(resetStore);
    const stopReset = onPrivateSessionReset(resetStore);
    return () => {
      stopPause();
      stopReset();
    };
  }, [enabled, resetStore]);

  useEffect(() => {
    return () => {
      if (toastTimer.current !== null && typeof window !== "undefined") {
        window.clearTimeout(toastTimer.current);
      }
    };
  }, []);

  const showToast = useCallback((kind: ToastMsg["kind"], text: string) => {
    setToastMsg({ kind, text });
    if (typeof window !== "undefined") {
      if (toastTimer.current !== null) window.clearTimeout(toastTimer.current);
      toastTimer.current = window.setTimeout(() => {
        toastTimer.current = null;
        setToastMsg(null);
      }, 4000);
    }
  }, []);

  const ensureLoaded = useCallback(() => {
    const store = storeRef.current;
    if (!enabled || store.loaded || store.loading || !privateReadsAllowed()) return;
    store.loading = true;
    const generation = privateSessionGeneration();
    void liveSkillDeps
      .list()
      .then((skills) => {
        if (!enabled || !privateReadsAllowed() || privateSessionGeneration() !== generation) return;
        const next = new Map(store.map);
        for (const skill of skills) next.set(skill.name, skill);
        store.map = next;
        store.loaded = true;
        emit();
      })
      .catch((error: unknown) => {
        if (libraryAbsent(error)) store.loaded = true;
      })
      .finally(() => {
        store.loading = false;
      });
  }, [enabled, emit]);

  const updateMap = useCallback(
    (name: string, value: WorkspaceSkillSummary | "deleted") => {
      const store = storeRef.current;
      const next = new Map(store.map);
      next.set(name, value);
      store.map = next;
      emit();
    },
    [emit],
  );

  const closeDrawer = useCallback(() => {
    setOpen(null);
    const opener = openerRef.current;
    openerRef.current = null;
    if (opener?.isConnected) opener.focus();
  }, []);

  const subscribe = useCallback((listener: () => void) => {
    const listeners = storeRef.current.listeners;
    listeners.add(listener);
    return () => listeners.delete(listener);
  }, []);

  const getSnapshot = useCallback((): ReadonlyMap<string, SkillDraftLiveState> => {
    return enabled ? storeRef.current.map : EMPTY_SKILL_MAP;
  }, [enabled]);

  const openSkill = useCallback(
    (name: string, revision?: number, opener?: HTMLElement | null) => {
      if (!enabled) return;
      openerRef.current = opener ?? null;
      setLibraryOpen(false);
      setOpen({ name, revision });
    },
    [enabled],
  );

  const openLibrary = useCallback(
    (opener?: HTMLElement | null) => {
      if (!enabled) return;
      openerRef.current = opener ?? null;
      setOpen(null);
      setLibraryOpen(true);
    },
    [enabled],
  );

  const closeLibrary = useCallback(() => {
    setLibraryOpen(false);
    const opener = openerRef.current;
    openerRef.current = null;
    if (opener?.isConnected) opener.focus();
  }, []);

  const source = useMemo<SkillDraftReviewSource | null>(() => {
    if (!enabled) return null;
    return {
      writable,
      openSkill,
      openLibrary,
      subscribe,
      getSnapshot,
      ensureLoaded,
    };
  }, [enabled, writable, openSkill, openLibrary, subscribe, getSnapshot, ensureLoaded]);

  const drawer = (
    <>
      {enabled && open !== null && (
        <SkillDrawer
          key={`${open.name}:${open.revision ?? "latest"}`}
          name={open.name}
          initialRevision={open.revision}
          initialTab="instructions"
          writable={writable}
          deps={liveSkillDeps}
          toast={showToast}
          onChanged={(skill) => updateMap(skill.name, skill)}
          onGone={(name) => {
            updateMap(name, "deleted");
            closeDrawer();
          }}
          onClose={closeDrawer}
        />
      )}
      {enabled && libraryOpen && open === null && (
        <WorkspaceSkillsDrawer
          writable={writable}
          deps={liveSkillDeps}
          toast={showToast}
          onSkillChanged={(skill) => updateMap(skill.name, skill)}
          onSkillGone={(name) => updateMap(name, "deleted")}
          onClose={closeLibrary}
        />
      )}
      <Toast msg={toastMsg} />
    </>
  );

  return { source, drawer };
}
