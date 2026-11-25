import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, ApiError } from "../../../lib/api";
import { useTranslate } from "../../../lib/locale";
import { resources } from "../../../lib/resources";
import type { Agent, MasterState } from "../../../lib/types";
import {
  effortMeterBars,
  effortShortLabel,
  filterModelOptions,
  masterModelsOptions,
  pickLanded,
  type MasterModelsView,
  type ModelOption,
} from "../../home/master";

function EffortMeter({ bars }: { bars: 0 | 1 | 2 | 3 }) {
  return (
    <span className="effort-meter" data-bars={bars} aria-hidden="true">
      <span className="effort-bar effort-bar-1" data-lit={bars >= 1 ? "true" : undefined} />
      <span className="effort-bar effort-bar-2" data-lit={bars >= 2 ? "true" : undefined} />
      <span className="effort-bar effort-bar-3" data-lit={bars >= 3 ? "true" : undefined} />
    </span>
  );
}

/**
 * CAD-2792: Upward-opening searchable Model picker and direct 3-bar Thinking
 * Effort selector docked inside the chat composer footer.
 */
export default function ComposerModelControls({
  master,
  row,
  conversation,
  readOnly,
  canPickModel = true,
  canPickEffort = true,
  compact = false,
  onRefreshState,
}: {
  master: MasterState | null | undefined;
  row?: Agent;
  conversation?: string;
  readOnly: boolean;
  canPickModel?: boolean;
  canPickEffort?: boolean;
  compact?: boolean;
  onRefreshState?: () => void;
}) {
  const t = useTranslate();
  const modelId = master?.model ?? row?.model ?? row?.model_reported ?? row?.model_configured ?? null;
  const masterLabel = master?.model_label ?? modelId;

  const [open, setOpen] = useState<"model" | "effort" | null>(null);
  const [query, setQuery] = useState("");
  const [highlight, setHighlight] = useState(0);
  const [view, setView] = useState<MasterModelsView | null>(null);
  const [catalogue, setCatalogue] = useState<MasterModelsView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<{ kind: "model" | "effort"; id: string } | null>(null);

  const modelTriggerRef = useRef<HTMLButtonElement | null>(null);
  const effortTriggerRef = useRef<HTMLButtonElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const [popPos, setPopPos] = useState<{
    left: number;
    bottom: number;
    maxWidth: number;
    maxHeight: number;
    width?: number;
    minWidth?: number;
  }>({ left: 8, bottom: 56, maxWidth: 336, maxHeight: 280 });

  useLayoutEffect(() => {
    if (!open || typeof window === "undefined") return;
    const place = () => {
      const trigger = open === "model" ? modelTriggerRef.current : effortTriggerRef.current;
      if (!trigger) return;
      const MARGIN = 8;
      const GAP = 6;
      const vw = window.innerWidth || 1024;
      const vh = window.innerHeight || 768;
      const popWidth =
        open === "model" ? Math.min(336, vw - MARGIN * 2) : Math.min(168, vw - MARGIN * 2);
      const rect = trigger.getBoundingClientRect();
      const left = Math.max(MARGIN, Math.min(rect.left, vw - popWidth - MARGIN));
      const bottom = Math.max(MARGIN, vh - rect.top + GAP);
      const maxHeight = Math.max(140, rect.top - GAP - MARGIN);
      setPopPos({
        left,
        bottom,
        maxWidth: vw - MARGIN * 2,
        maxHeight,
        ...(open === "model" ? { width: popWidth } : { minWidth: Math.min(136, vw - MARGIN * 2) }),
      });
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  const loadModels = useCallback(() => {
    setLoading(true);
    api
      .masterModels(conversation)
      .then((m) => {
        const parsed = masterModelsOptions(m);
        setView(parsed);
        setCatalogue(parsed);
        setError(null);
      })
      .catch((e: unknown) => {
        const message = e instanceof ApiError ? e.message : String(e);
        setError(message);
      })
      .finally(() => setLoading(false));
  }, [conversation]);

  useEffect(() => {
    if (!canPickModel || !modelId) return;
    let current = true;
    api
      .masterModels(conversation)
      .then((m) => {
        if (current) setCatalogue(masterModelsOptions(m));
      })
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [canPickModel, conversation, modelId]);

  const closeMenu = useCallback(
    (restoreFocus: boolean) => {
      const prev = open;
      setOpen(null);
      setQuery("");
      setHighlight(0);
      if (restoreFocus) {
        if (prev === "model") modelTriggerRef.current?.focus();
        else if (prev === "effort") effortTriggerRef.current?.focus();
      }
    },
    [open],
  );

  useEffect(() => {
    if (!open) return;
    if (open === "model") {
      setTimeout(() => searchInputRef.current?.focus(), 0);
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeMenu(true);
      }
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [open, closeMenu]);

  useEffect(() => {
    if (!pending) return;
    const current =
      pending.kind === "model"
        ? (master?.model ?? row?.model_reported ?? row?.model_configured)
        : (master?.effort ?? row?.effort_reported ?? row?.effort);
    if (pickLanded(pending.kind, pending.id, current)) setPending(null);
  }, [pending, master, row]);

  useEffect(() => {
    if (!pending) return;
    const timer = setTimeout(() => setPending(null), 15_000);
    return () => clearTimeout(timer);
  }, [pending]);

  const activeSource = view ?? catalogue;
  const activeEntry: ModelOption | undefined =
    activeSource?.models.find((m) => m.id === (modelId ?? activeSource.model)) ??
    activeSource?.models.find((m) => m.isDefault);
  const availableEfforts =
    activeEntry?.hasEffortList
      ? activeEntry.efforts
      : activeEntry && activeEntry.efforts.length > 0
        ? activeEntry.efforts
        : (activeSource?.efforts ?? []);

  const filteredModels = filterModelOptions(activeSource?.models ?? [], query);
  const clampedHighlight =
    filteredModels.length === 0 ? 0 : Math.min(highlight, filteredModels.length - 1);

  if (!canPickModel) return null;

  const displayModelLabel = activeEntry?.label ?? masterLabel ?? modelId ?? t("Default model");
  const activeEffort =
    master?.effort ??
    row?.effort ??
    row?.effort_reported ??
    activeSource?.effort ??
    activeEntry?.defaultEffort ??
    availableEfforts[0] ??
    null;

  const showEffortPill =
    canPickEffort && availableEfforts.length > 0 && activeEffort !== null;

  const toggle = (which: "model" | "effort") => {
    if (open === which) {
      closeMenu(false);
      return;
    }
    setQuery("");
    setHighlight(0);
    setOpen(which);
    loadModels();
  };

  const pick = (kind: "model" | "effort", id: string) => {
    closeMenu(true);
    setError(null);
    setPending({ kind, id });
    api
      .masterCommand(kind, id, undefined, undefined, conversation)
      .then(() => {
        if (onRefreshState) onRefreshState();
        else void resources.masterState.refresh();
        loadModels();
      })
      .catch((e: unknown) => {
        setPending(null);
        setError(e instanceof ApiError ? e.message : String(e));
      });
  };

  return (
    <div className={`composer-model-controls${compact ? " compact" : ""}`}>
      <div className="composer-pill-wrap">
        <button
          ref={modelTriggerRef}
          type="button"
          className="composer-pill"
          data-composer-model=""
          title={`${modelId ?? activeSource?.model ?? displayModelLabel} — ${t("Configured model — pick a model")}`}
          aria-haspopup="menu"
          aria-expanded={open === "model"}
          disabled={readOnly}
          onClick={() => toggle("model")}
        >
          <span className="composer-pill-label truncate">
            {pending?.kind === "model" ? `${pending.id}…` : displayModelLabel}
          </span>
          <span className="composer-pill-caret" aria-hidden="true">▾</span>
        </button>

        {open === "model" &&
          typeof document !== "undefined" &&
          createPortal(
            <>
              <button
                type="button"
                aria-hidden="true"
                tabIndex={-1}
                className="composer-pop-scrim"
                onClick={() => closeMenu(false)}
              />
              <div
                className="composer-pop composer-pop-model"
                role="menu"
                aria-label={t("pick a model")}
                style={popPos}
              >
                <div className="composer-pop-search">
                  <input
                    ref={searchInputRef}
                    type="text"
                    value={query}
                    data-composer-model-search=""
                    aria-label={t("Filter models")}
                    placeholder={t("Filter models…")}
                    onChange={(e) => {
                      setQuery(e.target.value);
                      setHighlight(0);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "ArrowDown") {
                        e.preventDefault();
                        setHighlight((i) => Math.min(i + 1, Math.max(0, filteredModels.length - 1)));
                      } else if (e.key === "ArrowUp") {
                        e.preventDefault();
                        setHighlight((i) => Math.max(0, i - 1));
                      } else if (e.key === "Enter" && filteredModels[clampedHighlight]) {
                        e.preventDefault();
                        if (!pending) pick("model", filteredModels[clampedHighlight].id);
                      }
                    }}
                  />
                </div>
                <div className="composer-pop-list">
                  {loading && !view && (
                    <p className="composer-pop-empty">{t("Reading models…")}</p>
                  )}
                  {error && !view && (
                    <p className="composer-pop-empty text-fail" role="alert">
                      {error}
                    </p>
                  )}
                  {activeSource && activeSource.models.length === 0 && (
                    <p className="composer-pop-empty">{t("The provider lists no models.")}</p>
                  )}
                  {activeSource && activeSource.models.length > 0 && filteredModels.length === 0 && (
                    <p className="composer-pop-empty">{t("No matching models")}</p>
                  )}
                  {filteredModels.map((m, idx) => {
                    const isCurrent =
                      activeSource?.model === m.id || modelId === m.id || masterLabel === m.label;
                    return (
                      <button
                        key={m.id}
                        type="button"
                        role="menuitemradio"
                        aria-checked={isCurrent}
                        className={`composer-pop-item${idx === clampedHighlight ? " is-highlighted" : ""}`}
                        disabled={!!pending}
                        onMouseEnter={() => setHighlight(idx)}
                        onClick={() => pick("model", m.id)}
                      >
                        <span className="composer-pop-item-text">
                          <span className="composer-pop-item-label">{m.label}</span>
                          {m.label !== m.id && <span className="composer-pop-item-id">{m.id}</span>}
                        </span>
                        {m.isDefault && (
                          <span className="composer-default-badge">{t("Default")}</span>
                        )}
                        {isCurrent && (
                          <span className="composer-pop-check" aria-label={t("current")}>
                            ✓
                          </span>
                        )}
                      </button>
                    );
                  })}
                </div>
              </div>
            </>,
            document.body,
          )}
      </div>

      {showEffortPill && (
        <div className="composer-pill-wrap">
          <button
            ref={effortTriggerRef}
            type="button"
            className="composer-pill composer-pill-effort"
            data-composer-effort=""
            title={t("Thinking effort — pick a level")}
            aria-haspopup="menu"
            aria-expanded={open === "effort"}
            disabled={readOnly}
            onClick={() => toggle("effort")}
          >
            <EffortMeter
              bars={effortMeterBars(pending?.kind === "effort" ? pending.id : activeEffort)}
            />
            <span className="composer-pill-label">
              {pending?.kind === "effort"
                ? `${t(effortShortLabel(pending.id))}…`
                : t(effortShortLabel(activeEffort))}
            </span>
            <span className="composer-pill-caret" aria-hidden="true">▾</span>
          </button>

          {open === "effort" &&
            typeof document !== "undefined" &&
            createPortal(
              <>
                <button
                  type="button"
                  aria-hidden="true"
                  tabIndex={-1}
                  className="composer-pop-scrim"
                  onClick={() => closeMenu(false)}
                />
                <div
                  className="composer-pop composer-pop-effort"
                  role="menu"
                  aria-label={t("pick a thinking level")}
                  style={popPos}
                >
                  <div className="composer-pop-list">
                    {loading && !view && (
                      <p className="composer-pop-empty">{t("Reading levels…")}</p>
                    )}
                    {error && !view && (
                      <p className="composer-pop-empty text-fail" role="alert">
                        {error}
                      </p>
                    )}
                    {availableEfforts.map((level) => {
                      const isCurrent =
                        activeSource?.effort === level ||
                        master?.effort === level ||
                        activeEffort === level;
                      return (
                        <button
                          key={level}
                          type="button"
                          role="menuitemradio"
                          aria-checked={isCurrent}
                          className="composer-pop-item"
                          disabled={!!pending}
                          onClick={() => pick("effort", level)}
                        >
                          <EffortMeter bars={effortMeterBars(level)} />
                          <span className="composer-pop-item-label">{t(effortShortLabel(level))}</span>
                          {isCurrent && (
                            <span className="composer-pop-check" aria-label={t("current")}>
                              ✓
                            </span>
                          )}
                        </button>
                      );
                    })}
                  </div>
                </div>
              </>,
              document.body,
            )}
        </div>
      )}

      {error && (
        <span className="text-micro text-fail break-words" role="alert">
          {error}
        </span>
      )}
    </div>
  );
}
