import SafeLink from "../../ui/SafeLink";
import { useState } from "react";
import { api } from "../../lib/api";
import type { IssueDetail } from "../../lib/types";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import { IconClose } from "../../ui/icons";
import { shownLinks } from "./model";
import { type IssuePageProps as Props } from "./issuePageProps";

const LINK_KINDS = [
  { value: "blocked_by", label: "Blocked by" },
  { value: "relates", label: "Related to" },
  { value: "parent", label: "Parent" },
  { value: "duplicate_of", label: "Duplicate of" },
];

export function Links({
  detail,
  readOnly,
  hrefFor,
  onWrite,
  onError,
}: {
  detail: IssueDetail;
  readOnly: boolean;
  hrefFor: (id: string) => string;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const [kind, setKind] = useState("relates");
  const [target, setTarget] = useState("");
  const rows = shownLinks(detail.links);
  const references = detail.refs.filter(
    (r) => r.kind === "note" || r.kind === "url",
  );
  const unlink = (unlinkKind: string, targetId: string) => {
    api
      .unlink(detail.id, unlinkKind, targetId, detail.rev)
      .then((r) => onWrite(r, `${detail.id} unlink ${unlinkKind} ${targetId}`))
      .catch((err) => onError(err, "unlink"));
  };
  return (
    <section className="card p-3.5 grid gap-2" aria-label="Links">
      <div className="slabel">Links</div>
      {rows.length === 0 && references.length === 0 && (
        <p className="text-secondary text-ink-500 m-0">No links yet.</p>
      )}
      <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
        {rows.map((row) => (
          <span key={`${row.label}-${row.id}`} className="contents">
            <dt className="slabel">{row.label}</dt>
            <dd className="text-secondary min-w-0 flex items-center gap-1.5">
              <span className="min-w-0 truncate">
                {row.missing ? (
                  <span className="num text-ink-500">{row.id}</span>
                ) : (
                  <Link className="lnk num" href={hrefFor(row.id)}>
                    {row.id}
                  </Link>
                )}
                {row.title ? (
                  <span className="text-ink-400"> · {row.title}</span>
                ) : null}
              </span>
              {row.unlinkKind && !readOnly && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="ml-auto shrink-0"
                  aria-label={`Unlink ${row.label} ${row.id}`}
                  title="Remove this link"
                  onClick={() => unlink(row.unlinkKind!, row.id)}
                >
                  <IconClose />
                </Button>
              )}
            </dd>
          </span>
        ))}
        {references.map((r, i) => (
          <span key={`reference-${i}`} className="contents">
            <dt className="slabel">
              {r.kind === "note" ? "Note" : "Reference"}
            </dt>
            <dd
              className="text-secondary min-w-0 truncate"
              title={r.url ?? r.path ?? r.label}
            >
              {r.url ? (
                <SafeLink className="lnk" href={r.url}>
                  {r.label ?? r.url}
                </SafeLink>
              ) : (
                <span>{r.label ?? r.path}</span>
              )}
            </dd>
          </span>
        ))}
      </dl>
      {!readOnly && (
        <form
          className="issue-link-form"
          onSubmit={(e) => {
            e.preventDefault();
            const t = target.trim();
            if (!t) return;
            api
              .link(detail.id, kind, t, detail.rev)
              .then((r) => {
                onWrite(r, `${detail.id} ${kind} ${t}`);
                setTarget("");
              })
              .catch((err) => onError(err, "link"));
          }}
        >
          <Select
            value={kind}
            aria-label="link type"
            options={LINK_KINDS}
            onChange={setKind}
          />
          <input
            className="field flex-1 min-w-0 num text-label"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="CAD-16"
            aria-label="link target"
          />
          <Button type="submit" disabled={!target.trim()}>
            Link
          </Button>
        </form>
      )}
    </section>
  );
}
