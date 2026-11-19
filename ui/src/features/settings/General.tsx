import { useId, useState } from "react";
import { useLocale } from "../../lib/locale";
import Select from "../../ui/Select";
import { setCompanyTimeZone, useTimeZones } from "../../lib/timeZone";
import TimeZoneRow from "./TimeZoneRow";
import Button from "../../ui/Button";
import DrawerShell from "../app-shell/shared/DrawerShell";
import DocumentTypes from "./DocumentTypes";
import type { Viewer } from "../projects/work";

/** Settings that apply to everyone in the workspace. */
export default function General({ documentPolicy }: { documentPolicy?: { hosted: boolean; viewer: Viewer; viewerSub: string | null } } = {}) {
  const id = useId();
  const { organizationLocale, canSetOrganizationLocale, t, setPreference } = useLocale();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const [policyOpen, setPolicyOpen] = useState(false);
  const zones = useTimeZones();

  const save = async (value: "en" | "zh-TW") => {
    setBusy(true);
    setError(false);
    try {
      await setPreference("organization", value);
    } catch {
      setError(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="px-4 lg:px-8 py-5 min-w-0 max-w-3xl">
      <header>
        <h1 className="text-section font-semibold text-ink-100">{t("General")}</h1>
        <p className="mt-1 text-secondary text-ink-400">{t("Settings for everyone in this workspace.")}</p>
      </header>
      <section className="mt-5 settings-rows" aria-labelledby={`${id}-language`}>
        <div className="grid gap-3 px-3 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(12rem,16rem)] sm:items-center">
          <div>
            <h2 id={`${id}-language`} className="text-secondary font-medium text-ink-100">{t("Default language")}</h2>
            <p className="mt-0.5 text-label text-ink-500">{t("For members who haven't chosen their own.")}</p>
          </div>
          <div className="min-w-0">
            <Select
              id={`${id}-locale`}
              aria-label={t("Default language")}
              full
              value={organizationLocale ?? ""}
              disabled={busy || !canSetOrganizationLocale}
              placeholder={t("Not set")}
              onChange={(value) => void save(value as "en" | "zh-TW")}
              options={[{ value: "en", label: t("English") }, { value: "zh-TW", label: "繁體中文" }]}
            />
            {error && <p role="alert" className="mt-2 text-label text-fail">{t("Could not save language preference.")}</p>}
          </div>
        </div>
        {zones.company && (
          <TimeZoneRow
            title={t("Company time zone")}
            hint={t("Every schedule runs in this zone. Members can choose another zone to see times in.")}
            value={zones.source === "workspace" ? zones.company : null}
            current={zones.company}
            disabled={!zones.canSetCompany}
            onSave={(zone) => (zone ? setCompanyTimeZone(zone) : Promise.resolve())}
          />
        )}
        {documentPolicy && (
          <div className="grid gap-3 px-3 py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
            <div><h2 className="text-secondary font-medium text-ink-100">{t("Document policy")}</h2><p className="mt-0.5 text-label text-ink-500">{t("Manage approval and retention rules for document types.")}</p></div>
            <Button onClick={() => setPolicyOpen(true)}>{t("Manage document types")}</Button>
          </div>
        )}
      </section>
      {documentPolicy && policyOpen && <DrawerShell kind="document-policy" label={t("Document policy")} title={t("Manage document types")} scrim="board" modal trapFocus tabs={[{ id: "types", label: t("Document types"), panel: <DocumentTypes {...documentPolicy} modalDrawer /> }]} onClose={() => setPolicyOpen(false)} />}
    </main>
  );
}
