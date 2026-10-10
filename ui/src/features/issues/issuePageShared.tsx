import { type ResourceState } from "../../lib/cache";
import Button from "../../ui/Button";
import { useLocale } from "../../lib/locale";

export function ReadNotice<T>({
  name,
  state,
  retry,
}: {
  name: string;
  state: ResourceState<T>;
  retry: () => void;
}) {
  const { t, locale } = useLocale();
  if (!state.error && !state.inFlight && state.status !== "loading")
    return null;
  const loading = state.inFlight || state.status === "loading";
  return (
    <div
      className="issue-read-notice"
      data-read-state={name}
      role={state.error ? "alert" : "status"}
    >
      <div>
        {state.error ? (
          <>
            <strong>
              {t(`${name[0].toUpperCase() + name.slice(1)} could not be loaded.`)}
            </strong>
            <p>
              {state.error}
              {state.data !== null
                ? ` ${t("Showing the last known information.")}`
                : ""}
            </p>
          </>
        ) : (
          <span>
            {t(state.data === null ? "Loading" : "Refreshing")}{locale === "zh-TW" ? "" : " "}{t(name)}…
          </span>
        )}
      </div>
      {state.error && (
        <Button loading={loading} onClick={retry}>
          {t("Retry")} {t(name)}
        </Button>
      )}
    </div>
  );
}
