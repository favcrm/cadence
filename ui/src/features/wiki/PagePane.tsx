import { useState } from "react";
import type { Route } from "../../lib/router";
import Button from "../../ui/Button";
import type { WikiPage } from "./api";
import { relTime } from "./history";
import { Note, WikiToolbar } from "./shared";
import { useTranslate } from "../../lib/locale";
import { beginPrivateOperation } from "../../lib/privateSession";
import { exportOriginal } from "./transfer";
import { baseName } from "./paths";
import { whoLabel } from "./hosted";
import PageText from "./PageText";

/** The same document toolbar and reader in Wiki and project Context. */
export default function PagePane({ page, navHref, readOnly, readOnlyReason, hosted, section, citedPage, viewerSub }: {
  page: WikiPage;
  navHref: (route: Route) => string;
  readOnly: boolean;
  readOnlyReason?: string | null;
  /** A hosted Worker board: it serves a pinned page's text, not the transfer export route. */
  hosted?: boolean;
  section?: string;
  citedPage?: number;
  viewerSub?: string | null;
}) {
  const t = useTranslate();
  const [downloadError, setDownloadError] = useState<string | null>(null);
  // Only the transport that really holds this revision's bytes offers the
  // download: the native daemon's verified export, or the text a hosted pinned
  // read already returned. A hosted historical attachment has no byte route,
  // so no button claims one.
  const hostedPinnedText = hosted === true && page.text !== undefined;
  const canDownload = Boolean(page.rev) && (hosted === true ? hostedPinnedText : true);
  const save = (blob: Blob) => {
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = baseName(page.path);
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  const download = async () => {
    if (!page.rev) return;
    if (hosted === true) {
      save(new Blob([page.text ?? ""], { type: "text/markdown;charset=utf-8" }));
      return;
    }
    const operation = beginPrivateOperation();
    try {
      const blob = await exportOriginal(page.path, page.rev, operation);
      operation.assertCurrent();
      save(blob);
    } catch (error) {
      if (!operation.signal.aborted) setDownloadError(error instanceof Error ? error.message : String(error));
    } finally {
      operation.dispose();
    }
  };
  return (
    <>
      <WikiToolbar path={page.path} navHref={navHref} actions={<>
          <Button href={navHref({ screen: "wiki", mode: "search", path: null, query: null })}>{t("Search")}</Button>
          <Button href={navHref({ screen: "wiki", mode: "history", path: page.path, query: null })}>{t("History")}</Button>
          {page.historical && canDownload && <Button onClick={() => void download()}>{t("Download original")}</Button>}
          {page.historical && <Button href={navHref({ screen: "wiki", mode: "browse", path: page.path, query: null })}>{t("Open the current revision")}</Button>}
          <Button variant="primary" disabled={readOnly}
            title={readOnly ? readOnlyReason ?? (page.historical ? t("Open the current revision to edit") : t("Sign in with editing access to edit")) : undefined}
            href={readOnly ? undefined : navHref({ screen: "wiki", mode: "edit", path: page.path, query: null })}>{t("Edit")}</Button>
      </>} />
      {readOnly && readOnlyReason && <Note warn>{readOnlyReason}</Note>}
      {page.historical && <Note>{t("Pinned revision")} <span className="num">{page.rev}</span>. {t("Open the current revision to read the live page.")}</Note>}
      {section && <p className="wk-meta">{t("Cited section")}: {section}</p>}
      {citedPage !== undefined && <p className="wk-meta">{t("Cited page")}: {citedPage}</p>}
      {downloadError && <Note warn>{downloadError}</Note>}
      {(page.edited_by || page.mtime) && <p className="wk-meta mb-4">
        {page.edited_by && <>{t("Edited by")} <b>{whoLabel(page.edited_by, viewerSub, t, hosted)}</b></>}
        {page.edited_by && page.mtime && " · "}{relTime(page.mtime)}
      </p>}
      <article className="md wk-md issue-reader">
        {page.text?.trim() ? <PageText path={page.path} text={page.text} /> : <p className="text-ink-500">{page.historical
          ? canDownload
            ? t("This revision has no page text. Download the pinned original.")
            : t("This board cannot serve this file’s historical bytes.")
          : t("This page is empty.")}</p>}
      </article>
    </>
  );
}
