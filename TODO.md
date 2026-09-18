# TODO

> **Related:** [[CLAUDE.md]] (what the tool does today), [[architecture.md]] (module layout + algorithm)

Open follow-ups only. Done entries are not kept — their history is in the git log, and the design that shipped is in the canonical docs.

## FEATURES

- **Wiki links in source comments go unchecked.** The crawl extracts links from
  `.md` files only, so a `// See [[foo.md#sec]]` breadcrumb in a `.rs` / `.cs` /
  `.ts` source file strands silently when the heading moves or the doc is
  renamed — the exact drift the tool exists to catch, just one file type over.
  Consumer repos put breadcrumbs in code deliberately (pspec's Authoring rules
  require it); a 2026-09 sweep there found nine broken links, **all nine** in
  `.rs` doc comments, none visible to `md-orphan`.

  Wants a source-comment extraction mode: scan configured source extensions for
  `[[...]]` spans and run them through the same resolve / anchor / style checks.
  Open questions — whether the link's file counts as reachable for orphan
  detection (a doc linked only from code is not orphaned), whether extraction
  needs to respect comment syntax per language or can stay a naive `[[...]]`
  scan, and what the walk cost is once the index is no longer `.md`-only
  (compare `--all-extensions`, ~30× on Unity-scale repos).
