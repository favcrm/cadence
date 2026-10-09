import { ApiError } from "../src/lib/api";
import { keepsPolling, readFailure } from "../src/features/app-shell/chat/AssistantOperations";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const err = (status: number, message: string) => new ApiError(message, status);

// An app that declares no assistant actions is a valid, permanent state: no error panel, no more polling.
check(readFailure(err(404, "no assistant actions declared")) === "none", "404 is the permanent no-actions state");
check(!keepsPolling(readFailure(err(404, "no assistant actions declared"))), "the no-actions state stops polling");
// A descriptor awaiting approval is calm and keeps polling so approval shows up.
check(readFailure(err(409, "app assistant is not consented")) === "consent", "409 not consented is the calm state");
check(keepsPolling(readFailure(err(409, "app assistant is not consented"))), "the not-consented state keeps polling");
// Everything else is a real failure with Retry (and keeps polling).
for (const [status, message] of [[500, "boom"], [503, "app assistant operation refused or unavailable"], [409, "app assistant operation refused or unavailable"], [403, "forbidden"]] as const) {
  check(readFailure(err(status, message)) === "error", `${status} ${message} is a real error`);
  check(keepsPolling(readFailure(err(status, message))), `${status} keeps polling for recovery`);
}
check(readFailure(new Error("network")) === "error", "a network failure is a real error");
console.log("assistant read outcome checks pass");
