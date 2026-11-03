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
    expect(text).not.toMatch(/\]\((?!https?:)[^)]*\)|\.\.\/|\b(?:src|ui|docs|worker-runtime)\/[\w/-]+\.\w+/);
  }
});

test("the session can list, load and read the skill", async () => {
  const [listed] = await companyOutputSkills.list();
  expect(listed?.name).toBe("company-outputs");
  expect((await companyOutputSkills.load("company-outputs"))?.body).toContain("# Company outputs");
  const reference = await companyOutputSkills.readResource?.("company-outputs", "references/quotation.md");
  expect(reference?.content).toContain("Draft view");
});
