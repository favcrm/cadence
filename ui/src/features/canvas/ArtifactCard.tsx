import { useContext } from "react";
import { useTranslate } from "../../lib/locale";
import { WriteGate } from "../auth/WriteGate";
import { requestSaveAsTemplateForArtifact } from "../templates/canvasTemplateDraft";
import { openArtifact, useCanvas, type ArtifactRef } from "./canvasStore";
import { artifactRef } from "./feed";
import { isBrowserPanelShown, useBrowserTab } from "./browserStore";
import type { ThreadEntry } from "../home/thread";
import { TurnCard, type CardDensity } from "../../ui/TurnCard";

export const KIND_LABEL: Record<string, string> = {
  md: "MD",
  pdf: "PDF",
  html: "HTML",
  image: "IMG",
  csv: "CSV",
  docx: "DOCX",
  xlsx: "XLSX",
  pptx: "PPTX",
};

export function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The thread's card for an artifact: kind tile, title, version and size. A
 *  click opens the drawer on that version; the card of the artifact on view
 *  reads "in canvas". The thread never holds the content. */
export default function ArtifactCard({
  entry,
  density = "full",
}: {
  entry: ThreadEntry;
  /** CAD-2440: the thread's density; the assistant panel is compact. */
  density?: CardDensity;
}) {
  const t = useTranslate();
  const canvas = useCanvas();
  const browser = useBrowserTab();
  const writeGate = useContext(WriteGate);
  const ref: ArtifactRef | null = artifactRef(entry.payload);
  if (!ref) return <p className="text-micro text-ink-500">· {t("Artifact unavailable")}</p>;
  const tab = canvas.open ? canvas.tabs.find((x) => x.id === canvas.active) : undefined;
  // The Browser tab, when in front, is what the canvas shows.
  const inCanvas = !isBrowserPanelShown(canvas, browser) && tab?.id === ref.id && tab.selected === ref.version;
  // CAD-2440: the card system's button shape; the markup, roles, labels
  // and hooks are the ones this card always had.
  const card = (
    <TurnCard
      shape="button"
      element="div"
      kind="artifact"
      density={density}
      data-kind="artifact"
      data-artifact={ref.id}
      open={inCanvas}
      role="button"
      tabIndex={0}
      aria-label={`${t("Open")} ${ref.title}`}
      onClick={() => openArtifact(ref)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          openArtifact(ref);
        }
      }}
      icon={KIND_LABEL[ref.kind] ?? "FILE"}
      subject={ref.title}
      meta={
        <span className="artmeta">
          v{ref.version}{ref.bytes === undefined ? "" : ` · ${sizeLabel(ref.bytes)}`}
        </span>
      }
      trailing={inCanvas ? <span className="artbadge">{t("in canvas")}</span> : <span className="artopen">{t("Open")}</span>}
    />
  );
  if (ref.kind !== "html") return card;
  return (
    <div className="grid gap-1" data-artifact-card-wrap={ref.id}>
      {card}
      <div className="flex justify-end">
        <button
          type="button"
          className="btn btn-ghost text-micro"
          data-artifact-save-template={ref.id}
          disabled={writeGate !== null}
          onClick={() => {
            requestSaveAsTemplateForArtifact(ref.id);
            openArtifact(ref);
          }}
        >
          {t("Save as Template")}
        </button>
      </div>
    </div>
  );
}
