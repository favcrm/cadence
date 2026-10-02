import type { ReactNode } from "react";
import { useHref } from "../../../lib/useLocation";
import ScreenOutlet from "./ScreenOutlet";
import type { ScreenPush } from "./screenProtocol";

/**
 * Host around a mounted app screen. The app screen is all a viewer sees: no
 * host tab bar (CAD-1040 removed the CAD-1026 toggle). `?screen=native` is
 * the operator's back door to the native workspace (WorkspaceApp renders
 * nothing here for a non-operator). Native unmounts the outlet, so its frame
 * and port close exactly as on a context switch, and dropping the parameter
 * mounts a fresh one.
 */
export default function ScreenHost({ projection, fallback }: { projection: ScreenPush; fallback: ReactNode }) {
  const href = useHref();
  const native = new URLSearchParams(href.split("?")[1] ?? "").get("screen") === "native";
  return native ? fallback : <ScreenOutlet projection={projection} fallback={fallback} />;
}
