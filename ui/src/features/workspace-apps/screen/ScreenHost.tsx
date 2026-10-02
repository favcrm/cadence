import type { ReactNode } from "react";
import Link from "../../../ui/Link";
import { useHref } from "../../../lib/useLocation";
import ScreenOutlet from "./ScreenOutlet";
import type { ScreenPush } from "./screenProtocol";

/** The same path with `screen=native` set, or removed for the default App view. */
function viewHref(href: string, native: boolean): string {
  const [path, search = ""] = href.split("?");
  const query = new URLSearchParams(search);
  if (native) query.set("screen", "native");
  else query.delete("screen");
  const s = query.toString();
  return path + (s ? `?${s}` : "");
}

/**
 * Host chrome around a mounted app screen (CAD-1026). `?screen=native` shows
 * the native workspace instead: the outlet unmounts, so its frame and port
 * close exactly as on a context switch, and App view mounts a fresh one.
 */
export default function ScreenHost({ projection, fallback }: { projection: ScreenPush; fallback: ReactNode }) {
  const href = useHref();
  const native = new URLSearchParams(href.split("?")[1] ?? "").get("screen") === "native";
  const tab = (label: string, toNative: boolean) => (
    <Link className="wa-tab" href={viewHref(href, toNative)} replace
      aria-current={native === toNative ? "page" : undefined}>{label}</Link>
  );
  return <>
    <nav className="wa-tabs wa-screen-view" aria-label="App view">
      {tab("App view", false)}
      {tab("Native controls", true)}
    </nav>
    {native ? fallback : <ScreenOutlet projection={projection} fallback={fallback} />}
  </>;
}
