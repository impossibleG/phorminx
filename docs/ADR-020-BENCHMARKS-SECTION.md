# ADR-020: Published benchmarks and the read-only Benchmarks page

- Status: accepted
- Date: 2026-10-19

## Decision

Recommendations for models, modes and engines are backed by benchmark runs
the team publishes in the repository, in `docs/benchmarks/`
([schema](benchmarks/README.md)):

- `benchmarks.json` is the latest report: runs for every feature (dictation,
  formatting, meetings, investigations, messaging, the app itself), each with
  its model, kind of backend, setup and metrics with units, plus example
  outputs, what was skipped and why, and plain nudges per feature ("Fastest",
  "Best quality", "Good default"). It describes no machine: no hardware, host
  or user names, addresses or paths;
- `reports/<id>.json` archives each published report; `history` lists them for
  the trend chart.

`phorminx-bench suite` measures one feature at a time with synthetic inputs
(generated speech and screenshots, written dictations) and merges the results.
`suite finalize` adds the verdict and key metrics, archives the report and
rebuilds the history.

The same JSON drives the app and the website. The app embeds the files present
at build time and shows them at once, then asks
`raw.githubusercontent.com/impossibleG/phorminx/main/docs/benchmarks/` for a
newer copy. That request is a download only: it sends no data about the
machine or its use and follows no redirects. Files are size-capped, report
names are restricted to `name.json`, and files with a newer schema are
skipped. If the download fails, the page shows the bundled copy and says why.
The website reads the same file at build time, with a committed snapshot as a
fallback.

The page is observational. Users never run benchmarks from the app. The setup
and repair recommender, which measures this computer, stays separate.

Charts are inline SVG drawn by `design/tauri-preview/benchmarks.js`, which
has no dependencies so the website can load the same file. They use the
app's design tokens in light and dark and load no external scripts. Every mark
is keyboard-focusable with a text tooltip. Identity is never shown by color
alone: models are named in legends, and cold, Portuguese or GPU bars are
textured. Every feature has a table view.

## Consequences

- A new recommendation is a reviewed commit of data. Clients pick it up
  without an app update.
- Results are a vibe check from one typical desktop, not the reader's
  computer, and the page says so. A busy GPU changes local-model results, so each run records
  decode speed and the report says when a model did not fit in GPU memory.
- The bundled copy keeps the page useful offline and in builds from forks.
