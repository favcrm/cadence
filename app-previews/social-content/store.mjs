import { createStudioFixture } from "./sdk.mjs";
import { studioFixture } from "./fixtures.mjs";
// Compatible component/CSS refresh preserves the one tab-local facade.
export const studio = createStudioFixture(studioFixture);
