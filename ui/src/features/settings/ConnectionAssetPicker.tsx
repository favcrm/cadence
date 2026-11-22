import { useEffect, useId, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import type { Connection, ConnectionAsset } from "../../lib/types";
import Button from "../../ui/Button";
import { IconCheck, IconLock, IconPin, IconRefresh } from "../../ui/icons";

export type AssetPinningProvider = "metaads" | "googleads";

export function isAssetPinningProvider(provider: string): provider is AssetPinningProvider {
  return provider === "metaads" || provider === "googleads";
}

interface ProviderCopy {
  heading: string;
  subtitle: string;
  pinnedFooter: string;
  unpinnedFooter: string;
  unpinnedLabel: string;
  emptyHeading: string;
  emptyDetail: string;
  restrictedNote: string;
  radioLabel: string;
  loadingLabel: string;
  pinButton: string;
  changedHint: string;
  unchangedHint: string;
  footerLock: string;
  loadError: string;
  saveError: string;
  successPrefix: string;
}

const PROVIDER_COPY: Record<AssetPinningProvider, ProviderCopy> = {
  metaads: {
    heading: "Meta ad account",
    subtitle: "Ad tools and spend mutations target one pinned Meta ad account for this connection.",
    pinnedFooter: "Used by Meta Ads tools",
    unpinnedFooter: "Ad spend mutations stay blocked until an ad account is pinned",
    unpinnedLabel: "No ad account pinned",
    emptyHeading: "No Meta ad accounts found",
    emptyDetail:
      "This Meta account has no accessible ad accounts yet. Grant ad account access in Meta Business Manager, then refresh.",
    restrictedNote:
      "No ad account can be pinned. Every returned ad account is marked inactive or unselectable.",
    radioLabel: "Meta ad accounts",
    loadingLabel: "Loading ad accounts",
    pinButton: "Pin ad account",
    changedHint:
      "Pin applies to Meta Ads tools and spend effects for this connection; it does not change Meta Ads.",
    unchangedHint: "This ad account is already pinned.",
    footerLock:
      "Only active selectable ad accounts can be pinned. Refreshing reads the provider and makes no change.",
    loadError: "The Meta ad account list is unavailable.",
    saveError: "The Meta ad account could not be pinned.",
    successPrefix: "Pinned. Meta Ads tools for this connection now use",
  },
  googleads: {
    heading: "Google Ads customer",
    subtitle: "Ad tools and spend mutations target one pinned Google Ads customer ID for this connection.",
    pinnedFooter: "Used by Google Ads tools",
    unpinnedFooter: "Ad spend mutations stay blocked until a customer ID is pinned",
    unpinnedLabel: "No customer pinned",
    emptyHeading: "No Google Ads customers found",
    emptyDetail:
      "This Google Ads connection returned no accessible customer accounts. Check Google Ads access, then refresh.",
    restrictedNote:
      "No customer ID can be pinned. Every returned customer is marked unselectable.",
    radioLabel: "Google Ads customers",
    loadingLabel: "Loading customers",
    pinButton: "Pin customer",
    changedHint:
      "Pin applies to Google Ads tools and spend effects for this connection; it does not change Google Ads.",
    unchangedHint: "This customer ID is already pinned.",
    footerLock:
      "Only accessible selectable customer IDs can be pinned. Refreshing reads the provider and makes no change.",
    loadError: "The Google Ads customer list is unavailable.",
    saveError: "The Google Ads customer could not be pinned.",
    successPrefix: "Pinned. Google Ads tools for this connection now use",
  },
};

function normalizeAssetComparison(provider: AssetPinningProvider, value: string): string {
  return provider === "googleads" ? value.trim().replaceAll("-", "") : value.trim();
}

function sameAssetValue(provider: AssetPinningProvider, a: string | null, b: string | null): boolean {
  if (a === null || b === null) return false;
  return normalizeAssetComparison(provider, a) === normalizeAssetComparison(provider, b);
}

function formatPermission(permission: string): string {
  const cleaned = permission.trim().replace(/[_-]+/g, " ");
  if (!cleaned) return "Unknown";
  return cleaned.charAt(0).toUpperCase() + cleaned.slice(1);
}

function formatPinnedAsset(label: string | null, value: string): string {
  return label && label !== value ? `${label} (${value})` : value;
}

/**
 * Self-serve execution-target picker for hosted Meta Ads and Google Ads connections.
 * Lists live accounts/customers via `GET /api/connections/:id/assets` and pins one at
 * connection level via `POST /api/connections/:id/asset`.
 */
export function ConnectionAssetPicker({ row, canWrite }: { row: Connection; canWrite: boolean }) {
  const t = useTranslate();
  const headingId = useId();
  const filterId = useId();
  const provider: AssetPinningProvider = isAssetPinningProvider(row.provider) ? row.provider : "metaads";
  const copy = PROVIDER_COPY[provider];

  const [assets, setAssets] = useState<ConnectionAsset[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [pinnedLabel, setPinnedLabel] = useState<string | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    setNote(null);
    setAssets(null);
    setSelected(null);
    setPinnedLabel(null);
    setDraft(null);
    setFilter("");
    api
      .connectionAssets(row.id)
      .then((reply) => {
        if (!alive) return;
        setAssets(reply.assets);
        setSelected(reply.selected);
        setDraft(reply.selected);
        const match = reply.selected
          ? reply.assets.find((entry) => sameAssetValue(provider, entry.value, reply.selected))
          : undefined;
        setPinnedLabel(match?.label ?? null);
      })
      .catch((e: unknown) => {
        if (!alive) return;
        setError(e instanceof ApiError && e.message ? e.message : copy.loadError);
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [row.id, reload, provider, copy.loadError]);

  const selectableAssets = (assets ?? []).filter((asset) => asset.selectable);
  const restrictedOnly = assets !== null && assets.length > 0 && selectableAssets.length === 0;
  const changed = draft !== null && !sameAssetValue(provider, draft, selected);
  const showFilter = assets !== null && assets.length > 8;
  const query = filter.trim().toLowerCase();
  const visibleAssets =
    assets === null
      ? []
      : query === ""
        ? assets
        : assets.filter(
            (asset) =>
              asset.label.toLowerCase().includes(query) ||
              asset.value.toLowerCase().includes(query) ||
              normalizeAssetComparison(provider, asset.value).toLowerCase().includes(query),
          );

  const save = () => {
    if (!draft || !changed || saving) return;
    setSaving(true);
    setError(null);
    setNote(null);
    api
      .connectionSelectAsset(row.id, { value: draft })
      .then((reply) => {
        setSelected(reply.asset.value);
        setPinnedLabel(reply.asset.label);
        setDraft(reply.asset.value);
        setNote(`${copy.successPrefix} ${formatPinnedAsset(reply.asset.label, reply.asset.value)}.`);
      })
      .catch((e: unknown) => {
        setError(e instanceof ApiError && e.message ? e.message : copy.saveError);
      })
      .finally(() => setSaving(false));
  };

  return (
    <div className="mt-4 border-t border-ink-700 pt-4" aria-labelledby={headingId}>
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="slabel">{t("Execution target")}</div>
          <h3 id={headingId} className="text-cardtitle font-medium text-ink-100 mt-1">
            {t(copy.heading)}
          </h3>
          <p className="text-label text-ink-400 mt-1 max-w-[36rem] break-words">{t(copy.subtitle)}</p>
        </div>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => setReload((value) => value + 1)}
          loading={loading}
          disabled={saving}
          icon={<IconRefresh />}
        >
          {t("Refresh")}
        </Button>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-x-2 gap-y-1 rounded border border-ink-700 bg-ink-850 px-2.5 py-2 min-w-0">
        <span className={`chip ${selected ? "bg-accent/10 text-accent" : "bg-ink-800 text-ink-400"}`}>
          {selected ? <IconCheck size={11} /> : <IconPin size={11} />}
          {selected ? t("Pinned") : t("Not pinned")}
        </span>
        {selected ? (
          <span className="flex flex-wrap items-center gap-x-2 gap-y-0.5 min-w-0">
            {pinnedLabel && pinnedLabel !== selected && (
              <span className="text-label text-ink-100 font-medium break-words">{pinnedLabel}</span>
            )}
            <code className="num text-label text-ink-200 break-all min-w-0">{selected}</code>
          </span>
        ) : (
          <span className="text-label text-ink-400">{t(copy.unpinnedLabel)}</span>
        )}
        <span className="ml-auto text-micro text-ink-500 break-words">
          {selected ? t(copy.pinnedFooter) : t(copy.unpinnedFooter)}
        </span>
      </div>

      {loading && (
        <div className="mt-3 grid gap-1.5" aria-label={t(copy.loadingLabel)} aria-busy="true">
          {[0, 1, 2].map((index) => (
            <div key={index} className="h-12 rounded border border-ink-700 bg-ink-850 animate-pulse" />
          ))}
        </div>
      )}

      {!loading && error && (
        <div className="mt-3 rounded border border-fail/30 bg-fail/5 px-3 py-2.5" role="alert">
          <p className="text-label text-ink-200 break-words">{t(error)}</p>
          <Button
            variant="secondary"
            size="sm"
            className="mt-2"
            onClick={() => setReload((value) => value + 1)}
            icon={<IconRefresh />}
          >
            {t("Try again")}
          </Button>
        </div>
      )}

      {!loading && !error && assets !== null && assets.length === 0 && (
        <div className="mt-3 rounded border border-dashed border-ink-700 px-3 py-3">
          <p className="text-label text-ink-200 font-medium">{t(copy.emptyHeading)}</p>
          <p className="text-label text-ink-400 mt-1 break-words">{t(copy.emptyDetail)}</p>
          <Button
            variant="secondary"
            size="sm"
            className="mt-2"
            onClick={() => setReload((value) => value + 1)}
            icon={<IconRefresh />}
          >
            {t("Refresh")}
          </Button>
        </div>
      )}

      {!loading && !error && showFilter && (
        <div className="mt-3 flex flex-wrap items-center gap-2 min-w-0">
          <label htmlFor={filterId} className="sr-only">
            {t("Filter accounts by name or ID")}
          </label>
          <input
            id={filterId}
            type="search"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder={t("Filter accounts by name or ID…")}
            className="min-w-0 flex-1 rounded border border-ink-700 bg-ink-900 px-2.5 py-1.5 text-label text-ink-100 placeholder:text-ink-500 focus:border-accent focus:outline-none"
          />
          <span className="text-micro text-ink-500 shrink-0">
            {t("Showing {shown} of {total}")
              .replace("{shown}", () => String(visibleAssets.length))
              .replace("{total}", () => String(assets.length))}
          </span>
        </div>
      )}

      {!loading && !error && assets !== null && assets.length > 0 && visibleAssets.length === 0 && (
        <p className="mt-3 text-label text-ink-400 break-words">
          {t("No matching accounts for this filter.")}
        </p>
      )}

      {!loading && !error && visibleAssets.length > 0 && (
        <div
          className={`mt-3 grid gap-1.5 ${showFilter ? "max-h-80 overflow-y-auto pr-0.5" : ""}`}
          role="radiogroup"
          aria-labelledby={headingId}
          aria-label={t(copy.radioLabel)}
        >
          {visibleAssets.map((asset, index) => {
            const usable = asset.selectable;
            const checked = sameAssetValue(provider, draft, asset.value);
            const inputId = `${headingId}-asset-${index}`;
            const metaParts = [
              asset.currency,
              asset.timezone,
              usable ? t("Selectable") : t("Unavailable to pin"),
            ].filter(Boolean);
            return (
              <label
                key={asset.value}
                htmlFor={inputId}
                className={`flex items-start gap-2 rounded border px-2.5 py-2 min-w-0 ${
                  checked ? "border-accent/50 bg-accent/10" : "border-ink-700 bg-ink-850"
                } ${usable && canWrite && !saving ? "cursor-pointer" : "opacity-70"}`}
              >
                <input
                  id={inputId}
                  type="radio"
                  name={`${headingId}-asset`}
                  value={asset.value}
                  checked={checked}
                  disabled={!usable || !canWrite || saving}
                  onChange={() => {
                    setDraft(asset.value);
                    setNote(null);
                  }}
                  className="mt-1 accent-[var(--color-accent)]"
                />
                <span className="min-w-0 flex-1">
                  <span className="text-label text-ink-100 block break-words font-medium">
                    {asset.label}
                  </span>
                  {asset.label !== asset.value && (
                    <code className="num text-micro text-ink-300 block break-all">
                      {asset.value}
                    </code>
                  )}
                  <span className="text-micro text-ink-500 block break-words">
                    {metaParts.join(" · ")}
                  </span>
                </span>
                <span
                  className={`chip shrink-0 ${
                    usable ? "bg-accent/10 text-accent" : "bg-ink-800 text-ink-500"
                  }`}
                >
                  {t(formatPermission(asset.permission))}
                </span>
              </label>
            );
          })}
        </div>
      )}

      {!loading && !error && restrictedOnly && (
        <p className="mt-2 flex items-start gap-1.5 text-label text-warn break-words" role="status">
          <IconLock className="mt-0.5 shrink-0" />
          <span>{t(copy.restrictedNote)}</span>
        </p>
      )}

      {!loading && !error && canWrite && selectableAssets.length > 0 && (
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <Button
            variant="primary"
            size="sm"
            onClick={save}
            loading={saving}
            disabled={!changed}
          >
            {changed ? t(copy.pinButton) : t("Pinned")}
          </Button>
          <span className="text-micro text-ink-500 break-words">
            {changed ? t(copy.changedHint) : t(copy.unchangedHint)}
          </span>
        </div>
      )}

      {!loading && !error && !canWrite && selectableAssets.length > 0 && (
        <p className="mt-2 text-micro text-ink-500">
          {t("Only the workspace owner can pin an account.")}
        </p>
      )}

      {note && (
        <p className="mt-2 flex items-start gap-1.5 text-label text-accent break-words" role="status">
          <IconCheck className="mt-0.5 shrink-0" />
          <span>{t(note)}</span>
        </p>
      )}

      <p className="mt-3 flex items-start gap-1.5 text-micro text-ink-500 break-words">
        <IconLock className="mt-0.5 shrink-0" />
        <span>{t(copy.footerLock)}</span>
      </p>
    </div>
  );
}
