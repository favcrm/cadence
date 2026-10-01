import { api } from "../../lib/api";
import { fmtBytes } from "../../lib/fmt";
import { Attach } from "./Attach";
import { type IssuePageProps as Props } from "./issuePageProps";

export function Evidence({
  id,
  images,
  files,
  readOnly,
  onWrite,
  onError,
}: {
  id: string;
  images: { name: string; size: number }[];
  files: { name: string; size: number }[];
  readOnly: boolean;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const empty = images.length === 0 && files.length === 0;
  return (
    <section className="grid gap-4">
      {empty && (
        <div className="card px-4 py-8 text-center">
          <div className="text-cardtitle font-semibold text-ink-100">
            No evidence yet
          </div>
          <p className="text-secondary text-ink-400 mt-1">
            Add screenshots, test results, and other supporting files.
          </p>
        </div>
      )}
      {images.length > 0 && (
        <div className="grid gap-2">
          <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
            Screenshots
          </h2>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {images.map((f) => (
              <figure key={f.name} className="card overflow-hidden m-0">
                <a
                  href={api.artifactUrl(id, f.name)}
                  target="_blank"
                  rel="noreferrer"
                >
                  <img
                    src={api.artifactUrl(id, f.name)}
                    alt={f.name}
                    className="w-full h-28 object-cover bg-ink-900"
                  />
                </a>
                <figcaption className="flex justify-between gap-2 px-2.5 py-2 text-label">
                  <span className="num truncate" title={f.name}>
                    {f.name}
                  </span>
                  <span className="text-ink-500 shrink-0">
                    {fmtBytes(f.size)}
                  </span>
                </figcaption>
              </figure>
            ))}
          </div>
        </div>
      )}
      {files.length > 0 && (
        <div className="card">
          <div className="slabel px-3 pt-2.5">Artifacts</div>
          <ul>
            {files.map((f) => (
              <li
                key={f.name}
                className="flex items-center gap-3 px-3 py-2 border-t border-ink-800"
              >
                <a
                  className="lnk num text-label truncate"
                  href={api.artifactUrl(id, f.name)}
                  target="_blank"
                  rel="noreferrer"
                  title={f.name}
                >
                  {f.name}
                </a>
                <span className="num text-micro text-ink-500 ml-auto shrink-0">
                  {fmtBytes(f.size)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {!readOnly && <Attach id={id} onWrite={onWrite} onError={onError} />}
    </section>
  );
}
