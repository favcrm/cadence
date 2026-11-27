import { useContext, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useTranslate } from "../../lib/locale";
import "../../ui/controls.css";
import { installedAppHref } from "../apps/appViewModel";
import { HireBlock, WriteGate } from "../auth/WriteGate";
import {
  readInstallationsGuarded,
  readInstallationsSnapshot,
  type Installation,
} from "../workspace-apps/workspaceApps";
import { openAppViewEditable } from "./canvasStore";

type WorksSection = "docs" | "sheets" | "decks";
type LauncherEntry = { section: WorksSection | "all"; key: string; label: string };

function findWorksApp(rows: Installation[] | null): Installation | null {
  if (!rows) return null;
  return (
    rows.find(
      (row) =>
        row.name === "works" &&
        row.approved === true &&
        row.executable === true &&
        (row.state === undefined || row.state === "active"),
    ) ?? null
  );
}

/**
 * Operator-only Canvas launcher for starting a new Doc, Sheet, or Deck in Works
 * (`view: "new"`, editable in place). Fails closed for non-operators, when
 * WriteGate/HireBlock is active, or when Works is not installed/approved.
 */
export default function WorksCanvasLauncher({ compact = false }: { compact?: boolean }) {
  const t = useTranslate();
  const writeGate = useContext(WriteGate);
  const hireBlock = useContext(HireBlock);
  const blocked = writeGate !== null || hireBlock !== null;

  const [works, setWorks] = useState<Installation | null>(() =>
    blocked ? null : findWorksApp(readInstallationsSnapshot()),
  );
  const [open, setOpen] = useState(false);
  const [alignRight, setAlignRight] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (blocked) {
      setWorks(null);
      return;
    }
    const controller = new AbortController();
    const cached = findWorksApp(readInstallationsSnapshot());
    if (cached) setWorks(cached);
    readInstallationsGuarded(controller.signal)
      .then((rows) => {
        if (!controller.signal.aborted) setWorks(findWorksApp(rows));
      })
      .catch(() => {
        // Fail closed silently if installations cannot be loaded.
      });
    return () => controller.abort();
  }, [blocked]);

  useEffect(() => {
    if (!open) return;
    const entries = Array.from(
      wrapRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [],
    );
    entries[0]?.focus();
    const onDown = (event: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  if (blocked || !works) return null;

  const items: LauncherEntry[] = [
    { section: "docs", key: "docs", label: t("New Doc") },
    { section: "sheets", key: "sheets", label: t("New Sheet") },
    { section: "decks", key: "decks", label: t("New Deck") },
    { section: "all", key: "import", label: t("Import file…") },
  ];

  const onKeyDown = (event: KeyboardEvent) => {
    if (!open) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
      triggerRef.current?.focus();
      return;
    }
    if (event.key === "Tab") {
      setOpen(false);
      return;
    }
    const entries = Array.from(
      wrapRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [],
    );
    const index = entries.indexOf(document.activeElement as HTMLElement);
    const next =
      event.key === "ArrowDown"
        ? (index + 1) % entries.length
        : event.key === "ArrowUp"
          ? (index - 1 + entries.length) % entries.length
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? entries.length - 1
              : -1;
    if (next >= 0) {
      event.preventDefault();
      entries[next]?.focus();
    }
  };

  const launch = (entry: LauncherEntry) => {
    setOpen(false);
    const baseHref = installedAppHref(works.name);
    if (!baseHref) return;
    const sectionQuery = works.navigation?.section_query || "works";
    openAppViewEditable({
      installId: works.install_id,
      tag: "works",
      contextId: "default",
      route: { section: entry.section, view: "new", record_id: null, segment_id: null },
      title: entry.label,
      appLabel: works.title || "Works",
      href: `${baseHref}?install=${encodeURIComponent(works.install_id)}&ctx=default&${encodeURIComponent(sectionQuery)}=${entry.section}`,
    });
  };

  return (
    <div className="row-menu cv-works-new" ref={wrapRef} onKeyDown={onKeyDown}>
      <button
        ref={triggerRef}
        type="button"
        className={compact ? "btn btn-ghost cv-works-new-btn cv-works-new-compact" : "btn btn-ghost cv-works-new-btn"}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={t("New in Works")}
        title={t("New in Works")}
        data-works-canvas-new
        onClick={() =>
          setOpen((was) => {
            if (!was) {
              const rect = triggerRef.current?.getBoundingClientRect();
              setAlignRight(
                Boolean(rect && rect.left + 176 > window.innerWidth - 8 && rect.right >= 176),
              );
            }
            return !was;
          })
        }
      >
        <span aria-hidden="true">+</span> {t("New")}
      </button>
      {open ? (
        <div
          className={
            alignRight
              ? "row-menu-pop cv-works-new-pop"
              : "row-menu-pop cv-works-new-pop cv-works-new-pop-start"
          }
          role="menu"
          aria-label={t("New in Works")}
        >
          {items.map((item) => (
            <button key={item.key} type="button" role="menuitem" onClick={() => launch(item)}>
              <span>{item.label}</span>
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
