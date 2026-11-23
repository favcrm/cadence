import { expect, test } from "bun:test";
import { COMPANY_OUTPUTS_SKILL } from "../src/company-outputs.generated.ts";
import { companyOutputSkills } from "../src/profile.ts";

test("every reference the skill names is carried, and none points into a repository", () => {
  const skill = COMPANY_OUTPUTS_SKILL as unknown as {
    body: string;
    resources: { path: string; content: string }[];
  };
  const carried = new Set(skill.resources.map((resource) => resource.path));
  const texts = [skill.body, ...skill.resources.map((resource) => resource.content)];
  for (const text of texts) {
    for (const [named] of text.matchAll(/references\/[\w-]+\.md/g)) expect(carried).toContain(named);
    expect(text).not.toContain("native detail");
    expect(text).not.toMatch(/`cadence \w+/);
    expect(text).not.toMatch(/\]\((?!https?:)[^)]*\)|\.\.\/|\b(?:src|ui|docs|scripts|skills|crates|tests|worker-runtime)\/[\w/-]+\.\w+|~\//);
  }
});

test("the session can list, load and read the skill", async () => {
  const listing = await companyOutputSkills.list();
  if (!listing.ok) throw listing.error;
  expect(listing.value.invalid).toEqual([]);
  const listed = listing.value.skills[0];
  expect(listed?.skill.name).toBe("company-outputs");
  expect(listed?.skill.body).toContain("# Company outputs");
  const info = listed?.resources.find((resource) => resource.path === "references/quotation.md");
  if (listed === undefined || info === undefined) throw new Error("skill reference is missing");
  const reference = await companyOutputSkills.readResource(listed.skill.name, info.path);
  if (!reference.ok) throw reference.error;
  expect(reference.value.content).toContain("Draft view");
});
