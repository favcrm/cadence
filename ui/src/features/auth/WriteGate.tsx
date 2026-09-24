import { createContext, useContext } from "react";
import { READ_ONLY_REASON } from "./gate";

/** Why writes are disabled on this board (see `writeBlock`), or null. */
export const WriteGate = createContext<string | null>(null);

/**
 * The reason to show beside a disabled write control: the board-wide
 * reason (read-only, or not signed in) when `disabled`, else null.
 */
export function useWriteBlock(disabled: boolean): string | null {
  const reason = useContext(WriteGate);
  return disabled ? (reason ?? READ_ONLY_REASON) : null;
}
