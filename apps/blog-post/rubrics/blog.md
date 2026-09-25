# Blog post review rubric

The reviewer is never the author (see `CONTRIBUTING.md`). Review the PR head commit, not a stale diff — the verdict binds to that SHA.

## Checklist

1. **Audience fit** — the post addresses the audience named in `brief.md` and follows the brief's angle.
2. **Factual accuracy, with sources** — every factual claim traces to a source in the brief's list; every source link resolves and supports the claim made next to it.
3. **No invented stats** — no number, percentage, date, benchmark, or quotation appears unless it comes from a cited source. An uncited number is an automatic REVISE.
4. **Protected terms** — every term in the brief's "Protected terms" list appears verbatim. Paraphrases or dropped qualifiers are an automatic REVISE.
5. **Structure** — one `#` title, scannable `##` sections, a "Sources" section, and no leftover template placeholders.
6. **Alt text on images** — every image has descriptive alt text a screen reader can use; each referenced file exists in the PR and is at most 1 MB.
7. **CI green** — the `ci` job passes on the PR head.

## Verdict format

Post exactly one verdict comment on the PR. PASS means every checklist item holds — say so per item, not "looks good". REVISE lists numbered reasons tied to checklist items.

```text
Verdict: PASS
Reviewed: <40-char head SHA>
```

```text
Verdict: REVISE
Reviewed: <40-char head SHA>
Reasons:
1. <checklist item> — <what is wrong and where>
```
