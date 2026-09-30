import { useMemo } from "react";
import Button from "../../../ui/Button";
import { navigate, useHref } from "../../../lib/useLocation";
import AppView from "./AppView";
import { appViewExamples, type AppViewExample } from "./examples";

/**
 * Dev-only app-views/v1 contract preview (CAD-861, toward CAD-811).
 *
 * Mounted by AppShell after its trusted installation load, only when
 * Vite's development flag is true and the URL carries `?contract-preview=crm`
 * or `?contract-preview=social-content`. It renders the worked
 * descriptors through the one shared renderer with the host's own
 * synthetic fixture rows — it is a contract demonstration inside the
 * trusted shell, not an installed Social surface and never live
 * customer data.
 *
 * A real descriptor landing later would still flow through the same
 * `parseAppView` gate before this component ever sees it.
 */

/** The preview state lives in the URL so a refresh, a copied link and
 *  browser back/forward all keep or drop it explicitly. */
export function contractPreviewKey(query: URLSearchParams): AppViewExample["key"] | null {
  const raw = query.get("contract-preview");
  return raw === "crm" || raw === "social-content" ? raw : null;
}

export function contractPreviewHref(href: string, key: AppViewExample["key"] | null): string {
  const [path, search] = href.split("?");
  const q = new URLSearchParams(search ?? "");
  if (key === null) {
    q.delete("contract-preview");
    q.delete("contract-preview-view");
  } else q.set("contract-preview", key);
  const s = q.toString();
  return path + (s ? `?${s}` : "");
}

export default function AppViewContractPreview({
  exampleKey,
  installationKind,
}: {
  exampleKey: AppViewExample["key"];
  /** The verified installation's own kind ("crm", "social-content", …)
   *  — shown so a foreign-kind fixture is visibly just a contract
   *  example, never an installed app of that kind. */
  installationKind: string;
}) {
  const href = useHref();
  const example: AppViewExample = appViewExamples[exampleKey];
  const views = example.descriptor.views;
  const current = useMemo(() => {
    const q = new URLSearchParams(href.split("?")[1] ?? "");
    const wanted = q.get("contract-preview-view");
    return views.find((v) => v.id === wanted) ?? views[0];
  }, [href, views]);

  const setParam = (patch: Record<string, string | null>) => {
    const [path, search] = href.split("?");
    const q = new URLSearchParams(search ?? "");
    for (const [k, v] of Object.entries(patch)) {
      if (v === null) q.delete(k);
      else q.set(k, v);
    }
    const s = q.toString();
    navigate(path + (s ? `?${s}` : ""), { replace: true });
  };

  return (
    <div className="av" data-contract-preview={exampleKey}>
      <div className="av-banner" role="note" aria-label="Development preview notice">
        <p className="text-label">
          <strong>Development preview — synthetic fixtures; no backend writes/sends.</strong>
        </p>
        <p className="text-micro text-ink-400">
          app-views/v1 contract example “{example.descriptor.title}” rendered inside the verified
          “{installationKind}” installation. The descriptor is data-only: identifiers, labels and
          formats — no executable fields, links, scope or actor claims. Switching the example below
          swaps the descriptor and its fixture rows only; the shell, chat and operational routes are
          unchanged.
        </p>
        <div className="av-tabs" role="group" aria-label="Contract example">
          {(["crm", "social-content"] as const).map((key) => (
            <button
              key={key}
              type="button"
              aria-pressed={key === exampleKey}
              className="app-outlet-tab"
              data-on={key === exampleKey || undefined}
              onClick={() => {
                const next = contractPreviewHref(href, key);
                const [path, search] = next.split("?");
                const q = new URLSearchParams(search ?? "");
                q.delete("contract-preview-view");
                const s = q.toString();
                navigate(path + (s ? `?${s}` : ""), { replace: true });
              }}
            >
              {appViewExamples[key].descriptor.app}
            </button>
          ))}
        </div>
      </div>

      <div className="av-tabs" role="group" aria-label={`${example.descriptor.app} views`}>
        {views.map((v) => (
          <button
            key={v.id}
            type="button"
            aria-pressed={v.id === current.id}
            className="app-outlet-tab"
            data-on={v.id === current.id || undefined}
            onClick={() => setParam({ "contract-preview-view": v.id })}
          >
            {v.title}
          </button>
        ))}
      </div>

      <AppView descriptor={example.descriptor} rows={example.rows} initialViewId={current.id} />

      <p className="num text-micro text-ink-500">
        Fixture keys come from the view's declared fields; undeclared keys, unknown formats and
        forbidden keys refuse at parse time.
      </p>

      <div>
        <Button
          size="sm"
          onClick={() => navigate(contractPreviewHref(href, null), { replace: true })}
        >
          Exit contract preview
        </Button>
      </div>
    </div>
  );
}
