import { useId, useMemo, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Select from "../../ui/Select";

/** Every zone this browser's IANA database names, with UTC always offered. */
function zoneOptions(extra: string | null): { value: string; label: string }[] {
  let names: string[] = [];
  try {
    names = Intl.supportedValuesOf("timeZone");
  } catch {
    // An older runtime: offer the common zones only.
    names = ["Asia/Hong_Kong", "Europe/London", "America/New_York", "America/Los_Angeles"];
  }
  const all = new Set(["UTC", ...names]);
  if (extra) all.add(extra);
  return [...all].sort().map((name) => ({ value: name, label: name }));
}

/**
 * One time-zone setting row. `followLabel` adds the "follow the company zone"
 * choice a person's own preference needs; the company setting has none.
 */
export default function TimeZoneRow({
  title,
  hint,
  value,
  current,
  followLabel,
  disabled,
  onSave,
}: {
  title: string;
  hint: string;
  /** The stored choice, or null for none. */
  value: string | null;
  /** The zone in force, shown on the follow option. */
  current: string | null;
  followLabel?: string;
  disabled?: boolean;
  onSave: (zone: string | null) => Promise<void>;
}) {
  const t = useTranslate();
  const id = useId();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const options = useMemo(() => {
    const zones = zoneOptions(value ?? current);
    return followLabel
      ? [{ value: "follow", label: `${followLabel}${current ? ` (${current})` : ""}` }, ...zones]
      : zones;
  }, [value, current, followLabel]);

  const save = async (next: string) => {
    setBusy(true);
    setError(null);
    try {
      await onSave(next === "follow" ? null : next);
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : t("Could not save the time zone."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="grid gap-3 px-3 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(12rem,16rem)] sm:items-center">
      <div>
        <h2 id={`${id}-title`} className="text-secondary font-medium text-ink-100">{title}</h2>
        <p className="mt-0.5 text-label text-ink-500">{hint}</p>
      </div>
      <div className="min-w-0">
        <Select
          id={`${id}-zone`}
          aria-label={title}
          full
          searchable
          value={value ?? (followLabel ? "follow" : current ?? "")}
          disabled={busy || disabled}
          placeholder={t("Not set")}
          onChange={(zone) => void save(zone)}
          options={options}
        />
        {error && <p role="alert" className="mt-2 text-label text-fail">{error}</p>}
      </div>
    </div>
  );
}
