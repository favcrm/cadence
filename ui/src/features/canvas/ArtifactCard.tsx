import { useTranslate } from "../../lib/locale";
import { openArtifact, type ArtifactRef } from "./canvasStore";
import { artifactRef } from "./feed";
import type { ThreadEntry } from "../home/thread";

const KIND_LABEL: Record<string, string> = { md: "Document", pdf: "PDF", html: "Report", image: "Image", csv: "Table" };

function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The thread's card for an artifact: title, kind, version — a click opens
 *  the drawer on that version. The thread never holds the content. */
export default function ArtifactCard({ entry }: { entry: ThreadEntry }) {
  const t = useTranslate();
  const ref: ArtifactRef | null = artifactRef(entry.payload);
  if (!ref) return <p className="text-micro text-ink-500">· {t("Artifact unavailable")}</p>;
  return (
    <button
      type="button"
      className="artifact-card card"
      data-kind="artifact"
      data-artifact={ref.id}
      onClick={() => openArtifact(ref)}
    >
      <span className="artifact-card-kind num">{t(KIND_LABEL[ref.kind] ?? "File")}</span>
      <span className="artifact-card-title">{ref.title}</span>
      <span className="artifact-card-meta num">
        v{ref.version} · {size(ref.bytes)}
      </span>
      <span className="artifact-card-open" aria-hidden>
        {t("Open")} ›
      </span>
    </button>
  );
}
