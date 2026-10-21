import { ApiError } from "../src/lib/api";
import { readFailure } from "../src/features/app-shell/chat/AssistantOperations";

function check(value: unknown, label: string): void { if (!value) throw new Error(label); }
const err = (status: number, message: string) => new ApiError(message, status);

// An app that declares no assistant actions is a valid, permanent state: no error panel.
check(readFailure(err(404, "no assistant actions declared")) === "none", "404 is the permanent no-actions state");
// A descriptor awaiting approval is a calm state.
check(readFailure(err(409, "app assistant is not consented")) === "consent", "409 not consented is the calm state");
// Everything else is a real failure with Retry.
for (const [status, message] of [[500, "boom"], [503, "app assistant operation refused or unavailable"], [409, "app assistant operation refused or unavailable"], [403, "forbidden"]] as const) {
  check(readFailure(err(status, message)) === "error", `${status} ${message} is a real error`);
}
check(readFailure(new Error("network")) === "error", "a network failure is a real error");
console.log("assistant read outcome checks pass");
