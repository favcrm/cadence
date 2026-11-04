import PageState from "../../ui/PageState";
import { useTranslate } from "../../lib/locale";
import { CustodyStatusPanel } from "./Connections";
import ProtectedManagement from "./ProtectedManagement";

/**
 * The key session, limits and native model source, on a board running on the
 * operator's own computer. They moved here so Connections lists only accounts.
 */
export default function AdvancedCustody({
  viewer,
}: {
  viewer: { operator: boolean | null; readOnly: boolean };
}) {
  const t = useTranslate();
  const canWrite = viewer.operator === true && !viewer.readOnly;
  return (
    <section className="px-4 lg:px-8 py-5 min-w-0" aria-labelledby="custody-page-title">
      <h1 id="custody-page-title" className="text-section font-medium text-ink-100">{t("Advanced custody")}</h1>
      <p className="text-label text-ink-400 mt-1 mb-4 break-words">{t("Where credentials are kept on this computer, and the limits on them.")}</p>
      {viewer.operator !== true ? (
        <PageState title={t("Operator access required")}>{t("Connections are available to the operator. Use Sign in if you are the operator.")}</PageState>
      ) : (
        <div className="grid gap-3 min-w-0">
          <CustodyStatusPanel canWrite={canWrite} />
          <ProtectedManagement canWrite={canWrite} />
        </div>
      )}
    </section>
  );
}
