---
name: website-builder
description: Build, review and publish a static website for a business owner through a fixed, script-tracked path with four owner checkpoints. Use when someone asks for a website, landing page or brochure site that must go live through Cadence web publishing.
---

# Website builder

This skill is a runnable playbook. The scripts in `scripts/` hold the run state
and the checks; you do the content work and talk to the owner. Read
`playbook.md` for the step table and commands, and `references/site-spec.md`
for the site contract.

## When to use

- The owner wants a site, landing page or brochure page published with `cadence web`.
- A run already exists (`run.json` in the run directory): resume it, do not restart it.

Do not use it for CRM writes, invoices, or sites that need a build toolchain.
Cadence web runs no npm or build step; the site must be plain static files.

## The four human checkpoints

The owner must answer these in their own words. Never answer them for them.

1. **Brief**: what the site is for, its pages, whether it needs a contact form, and the slug.
2. **Review**: the owner has looked at the built `dist/index.html` and approved it.
3. **Publish**: the owner approves the release in the browser, signed in to the selected workspace.
4. **Verify**: the owner confirms the live page is the one they approved.

## Stop conditions

- Never approve, publish, send or accept on the owner's behalf.
- Never invent clients, testimonials, prices, statistics, awards or results. Use only
  what the owner supplied; ask for anything missing.
- Ask the owner before any CRM write or message to a customer.
- Stop and report when a script refuses (exit 2) or a check fails (exit 1). Do not work
  around a refusal by editing `run.json` by hand.

## Driving the run

Each script prints one JSON object on stdout. Human diagnostics go to stderr.

```sh
python3 scripts/next.py --run RUN          # what step is next, and the question
python3 scripts/submit.py --run RUN --data DATA.json
```

After every command, turn the state into one plain sentence for the owner, for example:
"Your brief is recorded; next I need the text and images for the home page." Use
the `question` and `default` fields from `next.py`. Do not paste raw JSON at the owner.

## Scripts

- `next.py` creates the run if absent and prints the next step.
- `submit.py` records one owner-confirmed step and advances exactly one step. A resubmit
  of the same data is a no-op. Out-of-order, unknown or malformed input exits 2.
- `build.py` copies the source into `dist/` with digest-first asset names.
- `check.py` GETs the live site, compares its bytes with `dist/index.html`, reads the
  security headers and probes the contact form. `--record` writes the evidence during verify.
- `selftest.py` exercises every transition and the check parsing against a local fixture.

The scripts never call `cadence` and never write outside `--run` and `--out`.
