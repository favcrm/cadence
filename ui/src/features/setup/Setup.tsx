import Link from "../../ui/Link";

/**
 * First-run setup. The route exists so links to it work; the checklist,
 * master picker and repo path land with the setup screen itself.
 */
export default function Setup({ settingsHref }: { settingsHref: string }) {
  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[62rem] w-full">
      <h1 className="text-section font-semibold text-ink-100">Setup</h1>
      <p className="text-body text-ink-400 mt-2 max-w-[68ch]">
        Guided setup is not built yet. Agents and their model defaults are configured from the
        command line and under{" "}
        <Link href={settingsHref} className="lnk">
          Settings
        </Link>
        .
      </p>
    </main>
  );
}
