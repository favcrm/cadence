import { useState } from "react";
import type { Route } from "../../lib/router";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import type { WikiPage } from "./api";
import { relTime } from "./history";
import { Note, WikiToolbar } from "./shared";
import { useTranslate } from "../../lib/locale";
import { beginPrivateOperation } from "../../lib/privateSession";
import { exportOriginal } from "./transfer";
import { baseName } from "./paths";
import { whoLabel } from "./hosted";

/** The same document toolbar and reader in Wiki and project Context. */
export default function PagePane({ page, navHref, readOnly, section, citedPage, viewerSub }: {
  page: WikiPage;
  navHref: (route: Route) => string;
  readOnly: boolean;
  section?: string;
  citedPage?: number;
  viewerSub?: string | null;
}) {
  const t = useTranslate();
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const download = async () => {
    if (!page.rev) return;
    const operation = beginPrivateOperation();
    try {
      const blob = await exportOriginal(page.path, page.rev, operation);
      operation.assertCurrent();
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = baseName(page.path);
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
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
          {page.historical && page.rev && <Button onClick={() => void download()}>{t("Download original")}</Button>}
          {page.historical && <Button href={navHref({ screen: "wiki", mode: "browse", path: page.path, query: null })}>{t("Open the current revision")}</Button>}
          <Button variant="primary" disabled={readOnly}
            title={readOnly ? t("Sign in with editing access to edit") : undefined}
            href={readOnly ? undefined : navHref({ screen: "wiki", mode: "edit", path: page.path, query: null })}>{t("Edit")}</Button>
      </>} />
      {page.historical && <Note>{t("Historical revision")} {page.rev}. {t("This page is not the latest revision.")}</Note>}
      {section && <p className="wk-meta">{t("Cited section")}: {section}</p>}
      {citedPage !== undefined && <p className="wk-meta">{t("Cited page")}: {citedPage}</p>}
      {downloadError && <Note warn>{downloadError}</Note>}
      {(page.edited_by || page.mtime) && <p className="wk-meta mb-4">
        {page.edited_by && <>{t("Edited by")} <b>{whoLabel(page.edited_by, viewerSub, t)}</b></>}
        {page.edited_by && page.mtime && " · "}{relTime(page.mtime)}
      </p>}
      <article className="md wk-md issue-reader">
        {page.text?.trim() ? <Md text={page.text} /> : <p className="text-ink-500">{page.historical ? t("This revision has no page text. Download the pinned original.") : t("This page is empty.")}</p>}
      </article>
    </>
  );
}
