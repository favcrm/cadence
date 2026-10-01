import { useRef, useState } from "react";
import { api } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import type { IssueDetail } from "../../lib/types";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import { prRef } from "./model";
import { type IssuePageProps as Props } from "./issuePageProps";

export function PrPanel({
  detail,
  readOnly,
  onWrite,
  onError,
}: {
  detail: IssueDetail;
  readOnly: boolean;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
}) {
  const pr = prRef(detail.refs);
  const reports = detail.reports ?? [];
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  return (
    <div className="grid gap-5">
      <section className="card p-4 grid gap-3">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
          {pr?.label ?? "No pull request yet"}
        </h2>
        {pr ? (
          <>
            <p className="text-secondary text-ink-400 m-0">
              View live checks and merge queue status on the pull request.
            </p>
            {pr.href ? (
              <a
                className="lnk text-secondary break-all"
                href={pr.href}
                target="_blank"
                rel="noreferrer"
              >
                {pr.href}
              </a>
            ) : (
              <p className="text-secondary text-ink-400 m-0">
                This reference has no URL.
              </p>
            )}
          </>
        ) : (
          <>
            <p className="text-secondary text-ink-400 m-0">
              Link a pull request to keep its reviews and delivery evidence
              together.
            </p>
            {!readOnly && (
              <form
                className="issue-pr-form"
                onSubmit={async (e) => {
                  e.preventDefault();
                  const target = url.trim();
                  if (!target || pending.current || readOnly) return;
                  pending.current = true;
                  setBusy(true);
                  try {
                    const result = await api.addRef(
                      detail.id,
                      "pr",
                      { url: target },
                      undefined,
                      detail.rev,
                    );
                    onWrite(result, `${detail.id} ref pr`);
                    setUrl("");
                  } catch (e) {
                    onError(e, "ref");
                  } finally {
                    pending.current = false;
                    setBusy(false);
                  }
                }}
              >
                <label className="grid gap-1.5 min-w-0">
                  <span className="slabel">Pull request URL</span>
                  <input
                    className="field w-full min-w-0"
                    type="url"
                    required
                    disabled={busy}
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    placeholder="https://github.com/…/pull/1"
                  />
                </label>
                <Button type="submit" loading={busy} disabled={!url.trim()}>
                  Add PR
                </Button>
              </form>
            )}
          </>
        )}
      </section>
      <section className="grid gap-3">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">
          Review reports
        </h2>
        {reports.length === 0 ? (
          <p className="text-secondary text-ink-400 m-0">
            No review reports recorded on this issue.
          </p>
        ) : (
          reports.map((r) => (
            <article key={r.name} className="card p-4 grid gap-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <b className="text-ink-100">{r.agent ?? r.name}</b>
                <span className="chip bg-ink-800 text-ink-300">
                  {r.kind ?? "report"}
                </span>
              </div>
              {r.at && (
                <time className="num text-micro text-ink-500" dateTime={r.at}>
                  {fmtTime(r.at)}
                </time>
              )}
              {r.body && (
                <div className="issue-reader">
                  <Md text={r.body} />
                </div>
              )}
            </article>
          ))
        )}
      </section>
    </div>
  );
}
