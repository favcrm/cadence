import { useEffect } from "react";
import { resources } from "../../lib/resources";
import { useTranslate } from "../../lib/locale";
import Link from "../../ui/Link";
import { ResourceGate } from "../../ui/ResourceStatus";
import { useMaybeResource } from "../../lib/useResource";
import { connectionsNeedingAttention } from "./connectionsView";

/**
 * What an owner can usefully know about a hosted board. Updates are applied for
 * them and there is no daemon to report on, so this shows the build it is
 * running and points at anything that needs them.
 */
export default function HostedStatus({
  meta,
  canReadConnections,
  partialUpdates = false,
}: {
  meta: { build_commit?: string | null };
  canReadConnections: boolean;
  partialUpdates?: boolean;
}) {
  const t = useTranslate();
  const listRes = canReadConnections ? resources.connections : null;
  const list = useMaybeResource(listRes);
  useEffect(() => {
    if (listRes) void listRes.revalidate();
  }, [listRes]);
  const partialRes = canReadConnections ? resources.connectionsPartial : null;
  const partialState = useMaybeResource(partialRes);
  const partial = partialState?.data?.partial === true;
  const build = meta.build_commit ? meta.build_commit.slice(0, 12) : null;
  const rows = list?.data ?? null;
  const attention = rows ? connectionsNeedingAttention(rows) : 0;
  return (
    <main className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="status-title">
      <h1 id="status-title" className="text-section font-semibold text-ink-100">{t("Status")}</h1>
      <p className="mt-1 mb-4 text-secondary text-ink-400">{t("How this board is running.")}</p>
      <div className="settings-rows">
        <div className="settings-row">
          <span className="settings-row-text">
            <b>{t("Board")}</b>
            <small>
              {build
                ? t("Running · build {build} · updates apply automatically").replace("{build}", () => build)
                : t("Running · updates apply automatically")}
            </small>
          </span>
        </div>
        {partialUpdates && (
          <div className="settings-row">
            <span className="settings-row-text">
              <b>{t("Sync")}</b>
              <small>{t("Some projections refresh periodically in the background")}</small>
            </span>
          </div>
        )}
        {listRes && list && !rows && (
          <div className="settings-row">
            <span className="settings-row-text">
              <b>{t("Connections")}</b>
              <small>
                <ResourceGate
                  state={list}
                  loading="loading connections…"
                  failed="could not load connections"
                  onRetry={() => void listRes.refresh()}
                />
              </small>
            </span>
          </div>
        )}
        {rows && (
          <Link href="/settings/connections" className="settings-row">
            <span className="settings-row-text">
              <b>{t("Connections")}</b>
              <small>
                {partial
                  ? t("Some accounts couldn't be loaded")
                  : rows.length === 1
                    ? t("1 account")
                    : t("{count} accounts").replace("{count}", String(rows.length))}
              </small>
            </span>
            {(attention > 0 || partial) && <span className="chip bg-warn/10 text-warn">{t("Needs attention")}</span>}
          </Link>
        )}
      </div>
    </main>
  );
}
