import { useId } from "react";
import { useLocale } from "../lib/locale";

/**
 * Signed-out header language choice (CAD-1331). Browser-only: it stores a
 * local preference and never writes to the server or grants any authority.
 */
export default function LocalLanguage() {
  const { userLocale, t, setPreference } = useLocale();
  const id = useId();
  return (
    <span className="header-control-wrap">
      <label htmlFor={id} className="sr-only">{t("Language")}</label>
      <select
        id={id}
        value={userLocale ?? "browser"}
        onChange={(event) => {
          const value = event.target.value;
          void setPreference("user", value === "browser" ? null : value as "en" | "zh-TW");
        }}
        className="rounded border border-ink-700 bg-ink-900 px-1.5 text-micro text-ink-300"
      >
        <option value="browser">{t("Browser default")}</option>
        <option value="en">{t("English")}</option>
        <option value="zh-TW">繁體中文</option>
      </select>
    </span>
  );
}
