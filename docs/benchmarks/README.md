# Published benchmarks

`benchmarks.json` is the latest report. `reports/<id>.json` keeps every
published report in the same schema; `history` in `benchmarks.json` lists them
for the trend chart. The app's Benchmarks page and the website's
`/benchmarks` page both read these files. Neither measures anything on the
reader's computer.

This is a quick vibe check, not a lab test: one run on a typical desktop with
synthetic inputs only (speech generated with Windows voices, screenshots drawn
from fixed text, written sample dictations and public web pages). The files
describe no machine: no hardware, host or user names, addresses, recordings,
transcripts or file paths.

To produce a report, see [DEVELOPMENT.md](../DEVELOPMENT.md#run-locally).

## Schema (`schemaVersion` 1)

Readers ignore unknown fields and skip files with a newer `schemaVersion`.

| Field | Meaning |
| --- | --- |
| `schemaVersion` | `1`. Bumped when a field changes meaning. |
| `id`, `title`, `date` | Report id (its date), title and `YYYY-MM-DD`. |
| `appVersion`, `appCommit` | Release the measured code belongs to, and its commit. |
| `verdict` | `{headline, nudges: [{feature, kind, pick, why}]}`, or `null`. Nudges are plain suggestions such as `Fastest`, `Best quality` or `Good default`. |
| `summary` | Findings in Markdown, shown as text. |
| `features` | `[{id, title, status, note}]`; `status` is `measured`, `partial` or `skipped`. Ids: `dictation`, `formatting`, `meetings`, `investigations`, `messaging`, `app`. |
| `metrics` | `{metricId: {label, unit, better, description}}`; `better` is `lower` or `higher`. |
| `runs` | `[{id, feature, model, backend, config, repeats, metrics}]`. `config` holds the setup (`mode`, `passes`, `language`, `start` = `cold`/`warm`, `engine`, `task`, ...). `backend` is the kind of compute only (`CPU`, `GPU (Vulkan)`, `Ollama`). `metrics` maps a metric id to `{value, unit, p90?}`; values are medians of the repeats unless the metric says otherwise. |
| `examples` | `[{feature, runId, sample, language, input, output}]`: what a run produced for one input. |
| `skipped` | `[{feature, what, reason}]`: what was not measured, or measured with fewer repeats, and why. |
| `keyMetrics` | `[{key, label, unit, better, runId, metric}]`: headline numbers tracked across reports. |
| `history` | `[{id, date, appVersion, file, keyMetrics: {key: value}}]`, oldest first. |

Units are `s`, `ms`, `%`, `x` (real-time factor), `MiB`, `count`, `score`
(0-100) and `tok/s`.
