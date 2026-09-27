import { createFixtureSdk } from "./sdk.mjs";
import { fixtures } from "./fixtures.mjs";
// Separate from the React refresh boundary: UI/CSS edits retain this instance.
export const sdk = createFixtureSdk(fixtures);
