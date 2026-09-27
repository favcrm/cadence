import type { Route } from "../../lib/router";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import type { WikiPage } from "./api";
import { relTime } from "./history";
import { Crumbs } from "./shared";

/** The same document toolbar and reader in Wiki and project Context. */
export default function PagePane({ page, navHref, readOnly }: {
  page: WikiPage;
  navHref: (route: Route) => string;
  readOnly: boolean;
}) {
  const hrefFor = (path: string) => navHref({ screen: "wiki", mode: "browse", path: path || null, query: null });
  return (
    <>
      <div className="wk-bar">
        <Crumbs path={page.path} hrefFor={hrefFor} />
        <div className="wk-tools">
          <Button href={navHref({ screen: "wiki", mode: "search", path: null, query: null })}>Search</Button>
          <Button href={navHref({ screen: "wiki", mode: "history", path: page.path, query: null })}>History</Button>
          <Button variant="primary" disabled={readOnly}
            title={readOnly ? "Sign in with editing access to edit" : undefined}
            href={readOnly ? undefined : navHref({ screen: "wiki", mode: "edit", path: page.path, query: null })}>Edit</Button>
        </div>
      </div>
      {(page.edited_by || page.mtime) && <p className="wk-meta mb-4">
        {page.edited_by && <>Edited by <b>{page.edited_by}</b></>}
        {page.edited_by && page.mtime && " · "}{relTime(page.mtime)}
      </p>}
      <article className="md wk-md issue-reader">
        {page.text?.trim() ? <Md text={page.text} /> : <p className="text-ink-500">This page is empty.</p>}
      </article>
    </>
  );
}
