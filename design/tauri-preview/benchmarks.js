// Read-only Benchmarks view: renders docs/benchmarks/benchmarks.json (schema
// in phorminx-core's benchmark_report) with inline SVG charts. The app's
// Benchmarks page and the website's /benchmarks page load this same file, so
// it has no dependencies; `renderMarkdown` (markdown.js) is used when present
// and builds DOM nodes from text, never HTML.
(function (global) {
  'use strict';

  const SVG = 'http://www.w3.org/2000/svg';
  const SLOTS = 8;
  const FEATURE_ORDER = ['dictation', 'formatting', 'meetings', 'investigations', 'messaging', 'app'];
  const LABELS = {
    raw: 'Raw', light: 'Light', balanced: 'Balanced', strong: 'Strong', structured: 'Structured', custom: 'Custom',
    en: 'English', pt: 'Portuguese', warm: 'Warm', cold: 'Cold',
    instant: 'Instant', accurate: 'Accurate', loopback: 'Loopback', tor: 'Tor',
    small: 'Small', 'full-hd': 'Full HD', startup: 'Startup', 'settings-save': 'Settings save',
    dictation: 'Dictation', formatting: 'Formatting', meetings: 'Meetings', investigations: 'Investigations', messaging: 'Messaging', app: 'App',
  };
  const CONFIG_LABELS = { mode: 'Mode', passes: 'Passes', language: 'Language', start: 'Start', engine: 'Engine', size: 'Screenshot', transport: 'Transport' };
  /** Config keys offered as filter chips, in order. */
  const FILTER_KEYS = ['engine', 'mode', 'passes', 'language', 'start', 'size', 'transport'];

  // ---- pure helpers (tested by benchmarks.test.js) ----

  function mean(values) { return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null; }
  function label(value) { return LABELS[value] || String(value); }
  function unique(values) { return [...new Set(values)]; }
  function metric(run, name) { return run.metrics && run.metrics[name] ? run.metrics[name].value : null; }
  function passesLabel(count) { return Number(count) === 1 ? '1 pass' : `${count} passes`; }

  /** Formats a value in its unit. */
  function format(value, unit) {
    if (value === null || value === undefined || Number.isNaN(value)) return '—';
    switch (unit) {
      case 's': return value >= 100 ? `${Math.round(value)} s` : value >= 10 ? `${value.toFixed(1)} s` : `${value.toFixed(2)} s`;
      case 'ms': return value >= 10 ? `${Math.round(value)} ms` : `${value.toFixed(2)} ms`;
      case '%': return `${value >= 10 || value === 0 ? Math.round(value) : value.toFixed(1)}%`;
      case 'x': return `${value.toFixed(2)}×`;
      case 'MiB': return `${Math.round(value)} MiB`;
      case 'score': return `${Math.round(value)}/100`;
      case 'tok/s': return `${Math.round(value)} tok/s`;
      case 'count': return value.toFixed(1);
      default: return `${value} ${unit || ''}`.trim();
    }
  }

  /** Keeps runs of `feature` whose model and config match every active filter. */
  function filterRuns(runs, feature, filters) {
    return runs.filter((run) => run.feature === feature
      && (!filters.model || filters.model.has(run.model || ''))
      && FILTER_KEYS.every((key) => !filters[key] || !(key in run.config) || filters[key].has(String(run.config[key]))));
  }

  /** Runs matching every `config` pair exactly. */
  function where(runs, config) {
    return runs.filter((run) => Object.entries(config).every(([key, value]) => run.config[key] === value));
  }

  /** Mean of `name` over the runs that carry it. */
  function average(runs, name) { return mean(runs.map((run) => metric(run, name)).filter((value) => value !== null)); }

  /** A short description of a run's setup. */
  function describe(run) {
    const parts = [];
    if (run.model) parts.push(run.model);
    for (const [key, value] of Object.entries(run.config || {})) {
      if (key === 'passes') parts.push(passesLabel(value));
      else if (key !== 'resolution') parts.push(label(value));
    }
    if (run.backend && run.backend !== 'none') parts.push(run.backend);
    return parts.join(' · ');
  }

  /** Word-level diff: [{type: 'same' | 'ins' | 'del', text}]. */
  function wordDiff(before, after) {
    const left = before.split(/\s+/).filter(Boolean);
    const right = after.split(/\s+/).filter(Boolean);
    const normalize = (word) => word.toLowerCase().replace(/[^\p{L}\p{N}]/gu, '');
    const table = Array.from({ length: left.length + 1 }, () => new Array(right.length + 1).fill(0));
    for (let i = left.length - 1; i >= 0; i--) {
      for (let j = right.length - 1; j >= 0; j--) {
        table[i][j] = normalize(left[i]) === normalize(right[j]) && normalize(left[i])
          ? table[i + 1][j + 1] + 1
          : Math.max(table[i + 1][j], table[i][j + 1]);
      }
    }
    const parts = [];
    const push = (type, text) => {
      const last = parts[parts.length - 1];
      if (last && last.type === type) last.text += ` ${text}`;
      else parts.push({ type, text });
    };
    let i = 0;
    let j = 0;
    while (i < left.length && j < right.length) {
      if (normalize(left[i]) === normalize(right[j]) && normalize(left[i])) { push('same', right[j]); i++; j++; }
      else if (table[i + 1][j] >= table[i][j + 1]) push('del', left[i++]);
      else push('ins', right[j++]);
    }
    while (i < left.length) push('del', left[i++]);
    while (j < right.length) push('ins', right[j++]);
    return parts;
  }

  /** One point per history entry carrying `key`, oldest first. */
  function trendPoints(file, key) {
    return (file.history || [])
      .filter((entry) => entry.keyMetrics && typeof entry.keyMetrics[key] === 'number')
      .map((entry) => ({ id: entry.id, date: entry.date, appVersion: entry.appVersion, value: entry.keyMetrics[key] }))
      .sort((a, b) => a.date.localeCompare(b.date) || a.id.localeCompare(b.id));
  }

  /** Models in first-seen order across the whole file: their color slots. */
  function modelOrder(file) { return unique(file.runs.map((run) => run.model).filter(Boolean)); }

  function previousValue(file, key) {
    const points = trendPoints(file, key).filter((point) => point.date < file.date);
    return points.length ? points[points.length - 1].value : null;
  }

  // ---- DOM helpers ----

  function el(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  }

  function svgEl(tag, attributes = {}) {
    const element = document.createElementNS(SVG, tag);
    for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, String(value));
    return element;
  }

  function niceMax(value) {
    if (!(value > 0)) return 1;
    const magnitude = 10 ** Math.floor(Math.log10(value));
    for (const step of [1, 2, 2.5, 5, 10]) if (step * magnitude >= value) return step * magnitude;
    return 10 * magnitude;
  }

  function ticks(max, count = 4) { return Array.from({ length: count + 1 }, (_, index) => (max / count) * index); }

  // ---- render ----

  /**
   * Renders `collection` ({source, note, current}) into `root`.
   * options.loadReport(id) -> Promise<file> opens an archived report;
   * options.onRefresh() asks the host for newer results.
   */
  function render(root, collection, options = {}) {
    root.replaceChildren();
    root.classList.add('bench');
    const current = collection && collection.current;
    if (!current || !current.runs || !current.runs.length) {
      root.append(el('p', 'bench-empty', collection && collection.note ? collection.note : 'No benchmark results are available.'));
      return;
    }
    const tooltip = el('div', 'bench-tip');
    tooltip.setAttribute('role', 'tooltip');
    tooltip.hidden = true;
    const state = { file: current, tab: 'overview', filters: {}, trendKey: null, example: null };
    const features = () => FEATURE_ORDER.filter((id) => state.file.runs.some((run) => run.feature === id));

    const draw = (focus) => {
      const file = state.file;
      if (state.tab !== 'overview' && !features().includes(state.tab)) state.tab = 'overview';
      const colors = new Map(modelOrder(file).map((model, index) => [model, `var(--series-${(index % SLOTS) + 1})`]));
      const ctx = { file, colors, tooltip, state, draw, options, root };
      root.replaceChildren();
      root.append(header(collection, ctx, options), verdict(file), tabs(ctx, features()));
      const panel = el('div', 'bench-panel');
      panel.id = 'bench-panel';
      panel.setAttribute('role', 'tabpanel');
      panel.setAttribute('aria-labelledby', `bench-tab-${state.tab}`);
      if (state.tab === 'overview') overview(panel, ctx);
      else featureView(panel, ctx, state.tab);
      root.append(panel, tooltip, hatchDefinitions());
      if (focus) root.querySelector(focus)?.focus();
    };
    draw();
    return { redraw: () => draw() };
  }

  function header(collection, ctx, options) {
    const { file, state, draw } = ctx;
    const head = el('div', 'bench-head');
    const source = el('p', 'bench-source');
    source.setAttribute('role', 'status');
    source.textContent = state.file !== collection.current
      ? `Archived report from ${file.date}.`
      : collection.source === 'online'
        ? 'Latest published results from the Phorminx repository.'
        : collection.source === 'bundled'
          ? `Results included with this version${collection.note ? ` — ${collection.note}` : '.'}`
          : collection.note || 'Published results.';
    head.append(source);
    const history = (collection.current.history || []).slice().sort((a, b) => b.date.localeCompare(a.date));
    if (history.length > 1 && options.loadReport) {
      const picker = el('label', 'bench-picker');
      picker.append(el('span', '', 'Report'));
      const select = el('select');
      select.dataset.focus = 'report';
      for (const entry of history) {
        const option = el('option', '', `${entry.date} · v${entry.appVersion}`);
        option.value = entry.id;
        option.selected = entry.id === file.id;
        select.append(option);
      }
      select.addEventListener('change', async () => {
        const id = select.value;
        try {
          state.file = id === collection.current.id ? collection.current : await options.loadReport(id);
          state.filters = {};
          state.example = null;
        } catch (error) {
          source.textContent = `That report could not be opened: ${error}`;
          return;
        }
        draw('[data-focus="report"]');
      });
      picker.append(select);
      head.append(picker);
    }
    if (options.onRefresh) {
      const refresh = el('button', '', 'Check for newer results');
      refresh.type = 'button';
      refresh.addEventListener('click', () => options.onRefresh());
      head.append(refresh);
    }
    return head;
  }

  function verdict(file) {
    const card = el('section', 'bench-verdict');
    card.setAttribute('aria-label', 'Nudges');
    card.append(el('small', '', 'NUDGES'));
    const result = file.verdict;
    card.append(el('h2', '', result ? result.headline : 'No nudges in this report yet'));
    const nudges = (result && result.nudges) || [];
    if (nudges.length) {
      const list = el('ul', 'bench-nudges');
      for (const item of nudges) {
        const entry = el('li', 'bench-nudge');
        entry.append(
          el('span', 'bench-nudge-kind', `${label(item.feature)} · ${item.kind}`),
          el('strong', '', item.pick),
          el('span', 'bench-nudge-why', item.why),
        );
        list.append(entry);
      }
      card.append(list);
    }
    const where = el('p', 'bench-measured', `Measured ${file.date} on a typical desktop with Phorminx ${file.appVersion}, using synthetic speech, screenshots and dictations. A vibe check, not a lab test: your numbers will differ.`);
    card.append(where);
    if (file.summary) {
      const summary = el('div', 'bench-summary');
      if (global.renderMarkdown) summary.append(global.renderMarkdown(file.summary));
      else for (const paragraph of file.summary.split(/\n{2,}/)) summary.append(el('p', '', paragraph));
      card.append(summary);
    }
    return card;
  }

  function tabs(ctx, features) {
    const { file, state, draw } = ctx;
    const bar = el('div', 'bench-tabs');
    bar.setAttribute('role', 'tablist');
    bar.setAttribute('aria-label', 'Features');
    const ids = ['overview', ...features];
    const open = (id) => { state.tab = id; state.filters = {}; state.example = null; draw(`#bench-tab-${id}`); };
    ids.forEach((id, index) => {
      const status = (file.features || []).find((entry) => entry.id === id);
      const tab = el('button', 'bench-tab', id === 'overview' ? 'Overview' : (status ? status.title : label(id)));
      tab.type = 'button';
      tab.id = `bench-tab-${id}`;
      tab.setAttribute('role', 'tab');
      tab.setAttribute('aria-selected', String(state.tab === id));
      tab.setAttribute('aria-controls', 'bench-panel');
      tab.tabIndex = state.tab === id ? 0 : -1;
      if (status && status.status === 'partial') tab.title = 'Some measurements were skipped';
      tab.addEventListener('click', () => open(id));
      tab.addEventListener('keydown', (event) => {
        const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
        if (!step) return;
        event.preventDefault();
        open(ids[(index + step + ids.length) % ids.length]);
      });
      bar.append(tab);
    });
    return bar;
  }

  // ---- overview ----

  function overview(panel, ctx) {
    const { file } = ctx;
    const keys = file.keyMetrics || [];
    const values = new Map();
    for (const key of keys) {
      const run = file.runs.find((entry) => entry.id === key.runId);
      if (run && run.metrics[key.metric]) values.set(key.key, run.metrics[key.metric].value);
    }
    const row = el('div', 'bench-tiles');
    for (const key of keys.filter((entry) => values.has(entry.key)).slice(0, 8)) row.append(tile(file, key, values.get(key.key)));
    panel.append(row);
    const models = benchmarkedModels(ctx);
    if (models) panel.append(models);
    const grid = el('div', 'bench-grid');
    const formatting = file.runs.filter((run) => run.feature === 'formatting');
    if (formatting.length) {
      for (const chart of [
        formattingScatter(formatting, ctx),
        coldWarm(formatting, ctx, 'latency', 'Formatting: cold and warm', 'Median per model, averaged over modes, passes and languages.'),
        passesChart(formatting, ctx),
        languages(formatting.filter((run) => run.config.start === 'warm'), ctx, 'quality', 'Formatting: English and Portuguese', 'Quality score per model and language (warm).'),
      ]) if (chart) grid.append(chart);
    }
    grid.append(trendChart(ctx));
    panel.append(grid, statusList(file));
  }

  /** Slots a model can fill, by kind; the host checks again before saving. */
  const MODEL_SLOTS = [
    ['formatting', 'Formatting', 'language'],
    ['meetings', 'Meetings and investigations', 'language'],
    ['fallback', 'Meeting fallback', 'language'],
    ['translation', 'Live translation', 'language'],
    ['search', 'Search', 'embedding'],
  ];

  /** Local models the report measured, as {model, facts}. */
  function measuredModels(file) {
    return modelOrder(file)
      .filter((model) => file.runs.some((run) => run.model === model && run.backend === 'Ollama'))
      .map((model) => {
        const own = file.runs.filter((run) => run.model === model);
        const formatting = own.filter((run) => run.feature === 'formatting' && run.config.start === 'warm' && run.config.passes === 1);
        const summary = own.filter((run) => run.feature === 'meetings' && run.config.task === 'summary' && run.config.start === 'warm');
        const answer = own.filter((run) => run.feature === 'investigations' && run.config.task === 'answer' && run.config.start === 'warm');
        const facts = [];
        if (formatting.length) facts.push(`Formatting ${format(average(formatting, 'latency'), 's')}, quality ${format(average(formatting, 'quality'), 'score')}`);
        if (summary.length) facts.push(`meeting summary ${format(average(summary, 'answer'), 's')}`);
        if (answer.length) facts.push(`investigation answer ${format(average(answer, 'answer'), 's')}`);
        return { model, facts };
      });
  }

  /**
   * The benchmarked models, with "Use for…" and "Download" when the host
   * passes `options.models` ({entries(), use(slot, name), download(name)}).
   */
  function benchmarkedModels(ctx) {
    const { file, options, colors, state, root } = ctx;
    const models = measuredModels(file);
    if (!models.length) return null;
    const box = el('section', 'bench-models');
    box.setAttribute('aria-label', 'Benchmarked models');
    box.append(el('h3', '', 'Benchmarked models'));
    const status = el('p', 'bench-note', state.modelStatus || '');
    status.setAttribute('role', 'status');
    // The list redraws when the model registry changes; the last message stays.
    const say = (message) => {
      state.modelStatus = String(message);
      const live = root.querySelector('.bench-models [role="status"]');
      if (live) live.textContent = state.modelStatus;
    };
    const list = el('ul', 'bench-model-list');
    const actions = options && options.models;
    const entries = actions ? actions.entries() : [];
    const same = (left, right) => String(left).replace(/:latest$/, '') === String(right).replace(/:latest$/, '');
    for (const { model, facts } of models) {
      const item = el('li', 'bench-model');
      item.style.setProperty('--accent', colors.get(model) || 'var(--accent)');
      const text = el('div', 'bench-model-text');
      text.append(el('strong', '', model), el('span', '', facts.join(' · ') || 'Measured in this report.'));
      item.append(text);
      if (actions) {
        const entry = entries.find((candidate) => candidate.family === 'ollama' && same(candidate.name, model));
        const controls = el('div', 'bench-model-actions');
        if (!entry || !entry.installed) {
          const download = el('button', '', 'Download');
          download.type = 'button';
          download.addEventListener('click', async () => {
            download.disabled = true;
            say(`Downloading ${model}…`);
            try { say(await actions.download(model)); } catch (error) { say(error); download.disabled = false; }
          });
          controls.append(download);
        } else {
          const label = el('label', 'bench-picker');
          label.append(el('span', '', 'Use for'));
          const select = el('select');
          select.append(new Option('Choose a feature…', ''));
          for (const [slot, name, kind] of MODEL_SLOTS) if (entry.tags.includes(kind)) select.append(new Option(name, slot));
          select.addEventListener('change', async () => {
            const slot = select.value;
            if (!slot) return;
            select.disabled = true;
            try { say(await actions.use(slot, model)); } catch (error) { say(error); }
            select.value = '';
            select.disabled = false;
          });
          label.append(select);
          controls.append(label);
          if (entry.usedBy && entry.usedBy.length) controls.append(el('small', 'bench-model-used', `Used by ${entry.usedBy.join(', ')}`));
        }
        item.append(controls);
      }
      list.append(item);
    }
    box.append(list, status);
    return box;
  }

  function tile(file, key, value) {
    const box = el('div', 'bench-tile');
    const previous = previousValue(file, key.key);
    let detail = 'First report with this number';
    if (previous !== null) {
      const delta = value - previous;
      const better = key.better === 'lower' ? delta < 0 : delta > 0;
      detail = Math.abs(delta) < 1e-9 ? 'Same as the previous report' : `${delta > 0 ? '▲' : '▼'} ${format(Math.abs(delta), key.unit)} vs previous (${better ? 'better' : 'worse'})`;
    }
    box.append(el('span', 'bench-tile-label', key.label), el('strong', '', format(value, key.unit)), el('span', 'bench-tile-detail', detail));
    return box;
  }

  function statusList(file) {
    const box = el('section', 'bench-status');
    box.append(el('h3', '', 'What was measured'));
    const list = el('ul');
    for (const feature of file.features || []) {
      const item = el('li');
      const mark = feature.status === 'measured' ? '✓' : feature.status === 'partial' ? '◐' : '–';
      item.append(el('strong', '', `${mark} ${feature.title}`), document.createTextNode(` (${feature.status}). ${feature.note || ''}`));
      list.append(item);
    }
    box.append(list);
    const skipped = file.skipped || [];
    if (skipped.length) {
      const details = el('details');
      details.append(el('summary', '', `Not measured or reduced (${skipped.length})`));
      const items = el('ul');
      for (const skip of skipped) items.append(el('li', '', `${label(skip.feature)}: ${skip.what} — ${skip.reason}`));
      details.append(items);
      box.append(details);
    }
    return box;
  }

  // ---- feature views ----

  function featureView(panel, ctx, feature) {
    const { file, state } = ctx;
    const all = file.runs.filter((run) => run.feature === feature);
    const status = (file.features || []).find((entry) => entry.id === feature);
    if (status && status.note) panel.append(el('p', 'bench-note', status.note));
    panel.append(filterBar(all, ctx));
    const runs = filterRuns(file.runs, feature, state.filters);
    if (!runs.length) {
      panel.append(el('p', 'bench-empty', 'No runs match these filters.'));
      return;
    }
    const warm = runs.filter((run) => run.config.start !== 'cold');
    const byLanguage = (series) => series === 'Portuguese';
    const charts = {
      dictation: () => [
        scatterOf(warm, ctx, {
          title: 'Speed and accuracy', note: 'Each setup: time to first live text against word error rate. Down and left is better.',
          x: 'firstText', y: 'wer', key: describe,
        }),
        coldWarm(runs, ctx, null, 'Cold and warm start', 'Warm: first live text with the model loaded. Cold: model load plus first text.', { warm: 'firstText', cold: 'coldStart', group: (run) => `${run.model} · ${backendShort(run.backend)}` }),
        barsBy(warm, ctx, { title: 'Real-time factor', note: 'Compute per second of audio; below 1 keeps up. Hatched bars ran on the GPU (Vulkan).', metric: 'rtf', group: (run) => run.model, series: (run) => backendShort(run.backend), hatch: (series) => series === 'Vulkan' }),
        languages(warm, ctx, 'wer', 'English and Portuguese', 'Word error rate per model and language.'),
      ],
      formatting: () => [
        formattingScatter(runs, ctx),
        coldWarm(runs, ctx, 'latency', 'Cold and warm', 'Median per model over the selected modes, passes and languages.'),
        passesChart(runs, ctx),
        languages(warm.filter((run) => run.model), ctx, 'quality', 'English and Portuguese', 'Quality score per model and language (warm).'),
      ],
      meetings: () => [
        barsBy(where(runs, { task: 'transcribe' }), ctx, { title: 'Finalization', note: 'From Stop to a saved transcript, per engine and language.', metric: 'finalize', group: (run) => `${label(run.config.engine)} · ${run.model}`, series: (run) => label(run.config.language), hatch: byLanguage }),
        barsBy(where(runs, { task: 'transcribe' }), ctx, { title: 'Transcription accuracy', note: 'Word error rate on the meeting recording.', metric: 'wer', group: (run) => `${label(run.config.engine)} · ${run.model}`, series: (run) => label(run.config.language), hatch: byLanguage }),
        coldWarm(where(runs, { task: 'summary' }), ctx, 'answer', 'Summary time', 'A full meeting summary per model. Cold loads the model first.'),
        languages(where(runs, { task: 'summary', start: 'warm' }), ctx, 'answer', 'Summary: English and Portuguese', 'Warm summary time per model and language.'),
      ],
      investigations: () => [
        barsBy(where(warm, { task: 'ocr' }), ctx, { title: 'OCR speed', note: 'Reading every line of one screenshot on the CPU.', metric: 'ocrTime', group: (run) => `${label(run.config.size)} ${run.config.resolution}`, series: (run) => label(run.config.language), hatch: byLanguage }),
        barsBy(where(warm, { task: 'ocr' }), ctx, { title: 'OCR accuracy', note: 'Character error rate against the drawn text.', metric: 'cer', group: (run) => `${label(run.config.size)} ${run.config.resolution}`, series: (run) => label(run.config.language), hatch: byLanguage }),
        barsBy(where(runs, { task: 'link-preview' }), ctx, { title: 'Link previews', note: 'Fetching a public page with its image and icon. Varies with the network.', metric: 'fetch', group: (run) => run.config.site, series: () => 'Fetch' }),
        coldWarm(where(runs, { task: 'answer' }), ctx, 'answer', 'Assistant answers', 'Answering a question about an investigation, per model.'),
      ],
      messaging: () => [
        barsBy(runs, ctx, { title: 'Connecting', note: 'Bootstrap, invite redeem and first message. Hatched bars: cold start with an empty Tor state.', metric: ['bootstrap', 'redeem', 'firstMessage'], group: (run) => label(run.config.transport), series: (run) => label(run.config.start), hatch: (series) => series === 'Cold' }),
        barsBy(runs, ctx, { title: 'Message round trip', note: 'Send to delivered on an open connection.', metric: 'rtt', group: (run) => label(run.config.transport), series: (run) => label(run.config.start), hatch: (series) => series === 'Cold' }),
      ],
      app: () => [
        barsBy(where(runs, { task: 'startup' }), ctx, { title: 'Startup to window', note: 'Cold: first start of a new profile. Warm: a restart.', metric: 'startup', group: () => 'Phorminx', series: (run) => label(run.config.start), hatch: (series) => series === 'Cold' }),
        barsBy(runs, ctx, { title: 'Memory at idle', note: 'The app and its WebView2 processes, 10 s after startup.', metric: 'idleMemory', group: () => 'Phorminx', series: () => 'Private memory' }),
        barsBy(runs, ctx, { title: 'Settings save', note: 'Validating and atomically writing settings.toml.', metric: 'settingsSave', group: () => 'Settings', series: () => 'Save' }),
      ],
    };
    const grid = el('div', 'bench-grid');
    for (const chart of (charts[feature] || (() => []))()) if (chart) grid.append(chart);
    panel.append(grid, examples(runs, ctx, feature), table(runs, ctx));
    const skipped = (file.skipped || []).filter((skip) => skip.feature === feature);
    if (skipped.length) {
      const box = el('section', 'bench-status');
      box.append(el('h3', '', 'Not measured or reduced'));
      const list = el('ul');
      for (const skip of skipped) list.append(el('li', '', `${skip.what} — ${skip.reason}`));
      box.append(list);
      panel.append(box);
    }
  }

  function backendShort(backend) {
    if (!backend) return '';
    if (backend.startsWith('vulkan')) return 'Vulkan';
    if (backend === 'cpu') return 'CPU';
    return backend;
  }

  function filterBar(runs, ctx) {
    const { state, draw } = ctx;
    const bar = el('div', 'bench-filters');
    bar.setAttribute('role', 'group');
    bar.setAttribute('aria-label', 'Filter these charts');
    const group = (title, key, values, describeValue) => {
      if (values.length < 2) return;
      const box = el('div', 'bench-filter-group');
      box.append(el('span', 'bench-filter-label', title));
      for (const value of values) {
        const chip = el('button', 'bench-chip', describeValue(value));
        chip.type = 'button';
        chip.dataset.focus = `${key}:${value}`;
        chip.setAttribute('aria-pressed', String(!state.filters[key] || state.filters[key].has(value)));
        chip.addEventListener('click', () => {
          const active = new Set(state.filters[key] || values);
          if (active.has(value)) active.delete(value); else active.add(value);
          state.filters[key] = active.size === 0 || active.size === values.length ? null : active;
          draw(`[data-focus="${CSS.escape(`${key}:${value}`)}"]`);
        });
        box.append(chip);
      }
      bar.append(box);
    };
    group('Models', 'model', unique(runs.map((run) => run.model || '')).filter(Boolean), (value) => value);
    for (const key of FILTER_KEYS) {
      group(CONFIG_LABELS[key] || key, key, unique(runs.filter((run) => key in run.config).map((run) => String(run.config[key]))), (value) => key === 'passes' ? passesLabel(value) : label(value));
    }
    return bar;
  }

  function figure(title, description) {
    const box = el('figure', 'bench-figure');
    const caption = el('figcaption');
    caption.append(el('strong', '', title), el('span', '', description));
    box.append(caption);
    return box;
  }

  function attachTip(mark, tooltip, text) {
    mark.setAttribute('tabindex', '0');
    mark.setAttribute('role', 'img');
    mark.setAttribute('aria-label', text.replace(/\n/g, ', '));
    mark.classList.add('bench-mark');
    const show = () => {
      tooltip.textContent = text;
      tooltip.hidden = false;
      const host = tooltip.parentElement.getBoundingClientRect();
      const box = mark.getBoundingClientRect();
      const left = Math.min(Math.max(box.left + box.width / 2 - host.left, 130), host.width - 130);
      tooltip.style.left = `${left}px`;
      tooltip.style.top = `${box.top - host.top}px`;
    };
    const hide = () => { tooltip.hidden = true; };
    mark.addEventListener('pointerenter', show);
    mark.addEventListener('pointerleave', hide);
    mark.addEventListener('focus', show);
    mark.addEventListener('blur', hide);
  }

  function legend(entries) {
    const list = el('ul', 'bench-legend');
    for (const entry of entries) {
      const item = el('li');
      const swatch = el('span', `bench-swatch${entry.hatched ? ' hatched' : ''}`);
      swatch.style.setProperty('--swatch', entry.color);
      item.append(swatch, document.createTextNode(entry.label));
      list.append(item);
    }
    return list;
  }

  function plot(width, height, name) {
    return svgEl('svg', { viewBox: `0 0 ${width} ${height}`, role: 'group', 'aria-label': name });
  }

  /** The 45° texture laid over a filled mark: surface-colored lines. */
  function hatchDefinitions() {
    const svg = svgEl('svg', { width: 0, height: 0, 'aria-hidden': 'true', class: 'bench-defs' });
    const pattern = svgEl('pattern', { id: 'bench-hatch', width: 5, height: 5, patternUnits: 'userSpaceOnUse', patternTransform: 'rotate(45)' });
    pattern.append(svgEl('line', { x1: 0, y1: 0, x2: 0, y2: 5, class: 'bench-hatch-line' }));
    const defs = svgEl('defs');
    defs.append(pattern);
    svg.append(defs);
    return svg;
  }

  function yAxis(svg, frame, max, unit) {
    for (const value of ticks(max)) {
      const y = frame.bottom - (value / max) * (frame.bottom - frame.top);
      svg.append(svgEl('line', { x1: frame.left, x2: frame.right, y1: y, y2: y, class: value ? 'bench-grid-line' : 'bench-axis-line' }));
      const tick = svgEl('text', { x: frame.left - 8, y: y + 4, class: 'bench-tick', 'text-anchor': 'end' });
      tick.textContent = format(value, unit);
      svg.append(tick);
    }
  }

  function axisTitle(svg, x, y, text, rotate) {
    const title = svgEl('text', { x, y, class: 'bench-axis-title', 'text-anchor': 'middle', ...(rotate ? { transform: `rotate(-90 ${x} ${y})` } : {}) });
    title.textContent = text;
    svg.append(title);
  }

  function unitOf(ctx, name) { return ctx.file.metrics && ctx.file.metrics[name] ? ctx.file.metrics[name].unit : ''; }
  function nameOf(ctx, name) { return ctx.file.metrics && ctx.file.metrics[name] ? ctx.file.metrics[name].label : name; }
  function colorOf(ctx, model) { return model ? ctx.colors.get(model) : 'var(--text-secondary)'; }

  /** Dots for runs grouped by `key`: mean of x and y per group. */
  function scatterOf(runs, ctx, spec) {
    const groups = new Map();
    for (const run of runs) {
      if (metric(run, spec.x) === null || metric(run, spec.y) === null) continue;
      const key = spec.key(run);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(run);
    }
    const points = [...groups.entries()].map(([key, group]) => ({ key, model: group[0].model, x: average(group, spec.x), y: average(group, spec.y), runs: group }));
    if (!points.length) return null;
    const box = figure(spec.title, spec.note);
    const frame = { left: 60, right: 540, top: 16, bottom: 212 };
    const svg = plot(560, 252, spec.title);
    const xUnit = unitOf(ctx, spec.x);
    const yUnit = unitOf(ctx, spec.y);
    const maxX = niceMax(Math.max(...points.map((point) => point.x)));
    const rawMaxY = niceMax(Math.max(...points.map((point) => point.y)));
    const maxY = yUnit === 'score' ? 100 : rawMaxY;
    yAxis(svg, frame, maxY, yUnit);
    for (const value of ticks(maxX)) {
      const tick = svgEl('text', { x: frame.left + (value / maxX) * (frame.right - frame.left), y: frame.bottom + 16, class: 'bench-tick', 'text-anchor': 'middle' });
      tick.textContent = format(value, xUnit);
      svg.append(tick);
    }
    axisTitle(svg, (frame.left + frame.right) / 2, 248, nameOf(ctx, spec.x));
    axisTitle(svg, 12, (frame.top + frame.bottom) / 2, nameOf(ctx, spec.y), true);
    for (const point of points) {
      const dot = svgEl('circle', {
        cx: frame.left + (point.x / maxX) * (frame.right - frame.left),
        cy: frame.bottom - (point.y / maxY) * (frame.bottom - frame.top),
        r: 5.5, fill: colorOf(ctx, point.model), class: 'bench-dot',
      });
      const extra = spec.tipExtra ? spec.tipExtra(point.runs) : '';
      attachTip(dot, ctx.tooltip, `${point.key}\n${nameOf(ctx, spec.x)}: ${format(point.x, xUnit)}\n${nameOf(ctx, spec.y)}: ${format(point.y, yUnit)}${extra}`);
      svg.append(dot);
    }
    const models = unique(points.map((point) => point.model));
    box.append(svg, legend(models.map((model) => ({ label: model || 'No model (text as spoken)', color: colorOf(ctx, model) }))));
    return box;
  }

  function formattingScatter(runs, ctx) {
    return scatterOf(runs.filter((run) => run.config.start === 'warm'), ctx, {
      title: 'Speed and quality',
      note: 'One dot per model, mode and pass count (warm, averaged over languages). Up and left is better.',
      x: 'latency', y: 'quality',
      key: (run) => [run.model || 'Text as spoken', label(run.config.mode), passesLabel(run.config.passes)].join(' · '),
      tipExtra: (group) => `\nValidator pass rate: ${format(average(group, 'passRate'), '%')}${average(group, 'decodeRate') !== null ? `\nDecode speed: ${format(average(group, 'decodeRate'), 'tok/s')}` : ''}`,
    });
  }

  /** Grouped bars: one group per entry of `spec.groups`, one bar per series. */
  function bars(ctx, spec) {
    const { groups, series } = spec;
    const values = groups.flatMap((group) => series.map((entry) => group.values.get(entry))).filter((value) => value !== null && value !== undefined);
    if (!values.length) return null;
    const box = figure(spec.title, spec.note);
    const max = niceMax(Math.max(...values));
    // Many groups: angle their names so they do not collide.
    const angled = (480 / Math.max(groups.length, 1)) < 90;
    const frame = { left: 60, right: 540, top: 16, bottom: angled ? 180 : 206 };
    const svg = plot(560, 236, spec.title);
    yAxis(svg, frame, max, spec.unit);
    const band = (frame.right - frame.left) / Math.max(groups.length, 1);
    const barWidth = Math.max(4, Math.min(28, (band - 16) / series.length - 2));
    groups.forEach((group, index) => {
      const center = frame.left + band * (index + 0.5);
      series.forEach((entry, position) => {
        const value = group.values.get(entry);
        if (value === null || value === undefined) return;
        const height = Math.max(1, (value / max) * (frame.bottom - frame.top));
        const x = center - (series.length * (barWidth + 2)) / 2 + position * (barWidth + 2);
        const radius = Math.min(4, barWidth / 2, height);
        const d = `M${x},${frame.bottom}V${frame.bottom - height + radius}q0,-${radius} ${radius},-${radius}h${barWidth - 2 * radius}q${radius},0 ${radius},${radius}V${frame.bottom}Z`;
        const bar = svgEl('path', { d, fill: group.color, class: 'bench-bar' });
        const tip = group.tips && group.tips.get(entry);
        attachTip(bar, ctx.tooltip, `${group.label}\n${entry}: ${format(value, spec.unit)}${tip ? `\n${tip}` : ''}`);
        svg.append(bar);
        if (spec.hatch && spec.hatch(entry)) svg.append(svgEl('path', { d, fill: 'url(#bench-hatch)', class: 'bench-hatch' }));
      });
      const short = group.label.length > 24 ? `${group.label.slice(0, 23)}…` : group.label;
      const name = angled
        ? svgEl('text', { x: center, y: frame.bottom + 12, class: 'bench-tick', 'text-anchor': 'end', transform: `rotate(-30 ${center} ${frame.bottom + 12})` })
        : svgEl('text', { x: center, y: frame.bottom + 15, class: 'bench-tick', 'text-anchor': 'middle' });
      name.textContent = short;
      if (short !== group.label) { const title = svgEl('title'); title.textContent = group.label; name.append(title); }
      svg.append(name);
    });
    box.append(svg);
    if (series.length > 1) box.append(legend(series.map((entry) => ({ label: entry, color: 'var(--text-secondary)', hatched: Boolean(spec.hatch && spec.hatch(entry)) }))));
    const models = unique(groups.map((group) => group.model)).filter(Boolean);
    if (models.length > 1) box.append(legend(models.map((model) => ({ label: model, color: colorOf(ctx, model) }))));
    return box;
  }

  /** Bars of one metric (or several, as series) grouped by `spec.group`. */
  function barsBy(runs, ctx, spec) {
    const metrics = Array.isArray(spec.metric) ? spec.metric : [spec.metric];
    const carrying = runs.filter((run) => metrics.some((name) => metric(run, name) !== null));
    if (!carrying.length) return null;
    const unit = unitOf(ctx, metrics[0]);
    const seriesKey = (series, name) => (metrics.length > 1 ? `${series} · ${nameOf(ctx, name)}` : series);
    const seriesNames = unique(carrying.map(spec.series)).flatMap((series) => metrics.filter((name) => carrying.some((run) => spec.series(run) === series && metric(run, name) !== null)).map((name) => seriesKey(series, name)));
    const groups = unique(carrying.map(spec.group)).map((name) => {
      const own = carrying.filter((run) => spec.group(run) === name);
      const values = new Map();
      for (const series of unique(own.map(spec.series))) {
        const subset = own.filter((run) => spec.series(run) === series);
        for (const metricName of metrics) {
          const value = average(subset, metricName);
          if (value !== null) values.set(seriesKey(series, metricName), value);
        }
      }
      return { label: name, model: own[0].model, color: colorOf(ctx, own[0].model), values };
    });
    return bars(ctx, { title: spec.title, note: spec.note, groups, series: seriesNames, unit, hatch: spec.hatch ? (entry) => spec.hatch(entry.split(' · ')[0]) : null });
  }

  /** Warm against cold per model (or per `options.group`). */
  function coldWarm(runs, ctx, name, title, note, options = {}) {
    const warmName = options.warm || name;
    const coldName = options.cold || name;
    const group = options.group || ((run) => run.model);
    const names = unique(runs.filter((run) => run.model).map(group));
    const groups = names.map((groupName) => {
      const own = runs.filter((run) => run.model && group(run) === groupName);
      const values = new Map();
      const warm = average(own.filter((run) => run.config.start === 'warm'), warmName);
      const cold = average(own.filter((run) => run.config.start === 'cold'), coldName);
      if (warm !== null) values.set('Warm', warm);
      if (cold !== null) values.set('Cold', cold);
      const tips = new Map();
      const rate = average(own.filter((run) => run.config.start === 'warm'), 'decodeRate');
      if (rate !== null) tips.set('Warm', `Decode speed: ${format(rate, 'tok/s')}`);
      return { label: groupName, model: own[0].model, color: colorOf(ctx, own[0].model), values, tips };
    });
    return bars(ctx, { title, note, groups, series: ['Warm', 'Cold'], unit: unitOf(ctx, warmName), hatch: (entry) => entry === 'Cold' });
  }

  function languages(runs, ctx, name, title, note) {
    return barsBy(runs.filter((run) => run.model), ctx, { title, note, metric: name, group: (run) => run.model, series: (run) => label(run.config.language), hatch: (series) => series === 'Portuguese' });
  }

  function passesChart(runs, ctx) {
    const warm = runs.filter((run) => run.config.start === 'warm' && run.model);
    const counts = unique(warm.map((run) => run.config.passes)).filter((count) => count !== undefined).sort((a, b) => a - b);
    if (counts.length < 2) return null;
    const box = figure('Effect of a second pass', 'Left: latency by pass count. Right: share of words the second pass still changed.');
    const models = unique(warm.map((run) => run.model));
    const pair = el('div', 'bench-pair');
    const panel = (title, valueFor, unit, max) => {
      const frame = { left: 54, right: 262, top: 16, bottom: 190 };
      const svg = plot(276, 214, title);
      yAxis(svg, frame, max, unit);
      const xFor = (count) => frame.left + 14 + ((count - counts[0]) / (counts[counts.length - 1] - counts[0])) * (frame.right - frame.left - 28);
      for (const count of counts) {
        const tick = svgEl('text', { x: xFor(count), y: frame.bottom + 16, class: 'bench-tick', 'text-anchor': 'middle' });
        tick.textContent = passesLabel(count);
        svg.append(tick);
      }
      for (const model of models) {
        const xy = counts.map((count) => [count, valueFor(model, count)]).filter(([, value]) => value !== null)
          .map(([count, value]) => [xFor(count), frame.bottom - (value / max) * (frame.bottom - frame.top), count, value]);
        if (xy.length > 1) svg.append(svgEl('polyline', { points: xy.map(([x, y]) => `${x},${y}`).join(' '), stroke: colorOf(ctx, model), class: 'bench-line' }));
        for (const [x, y, count, value] of xy) {
          const dot = svgEl('circle', { cx: x, cy: y, r: 4.5, fill: colorOf(ctx, model), class: 'bench-dot' });
          attachTip(dot, ctx.tooltip, `${model} · ${passesLabel(count)}\n${title}: ${format(value, unit)}`);
          svg.append(dot);
        }
      }
      const wrap = el('div');
      wrap.append(el('span', 'bench-panel-title', title), svg);
      return wrap;
    };
    const latencyFor = (model, count) => average(warm.filter((run) => run.model === model && run.config.passes === count), 'latency');
    const changeFor = (model, count) => (count === counts[0] ? 0 : average(warm.filter((run) => run.model === model && run.config.passes === count), 'laterPassChange'));
    const maxLatency = niceMax(Math.max(...models.flatMap((model) => counts.map((count) => latencyFor(model, count) || 0))));
    const maxChange = Math.min(100, niceMax(Math.max(1, ...models.flatMap((model) => counts.map((count) => changeFor(model, count) || 0)))));
    pair.append(panel('Latency', latencyFor, 's', maxLatency), panel('Words changed', changeFor, '%', maxChange));
    box.append(pair, legend(models.map((model) => ({ label: model, color: colorOf(ctx, model) }))));
    return box;
  }

  function trendChart(ctx) {
    const { file, state, tooltip, draw } = ctx;
    const keys = (file.keyMetrics || []).filter((key) => trendPoints(file, key.key).length);
    const box = figure('Trend across reports', 'One key number per published report.');
    if (!keys.length) {
      box.append(el('p', 'bench-note', 'The trend appears once reports are archived.'));
      return box;
    }
    const key = keys.find((entry) => entry.key === state.trendKey) || keys[0];
    const picker = el('label', 'bench-picker');
    picker.append(el('span', '', 'Number'));
    const select = el('select');
    select.dataset.focus = 'trend';
    for (const entry of keys) {
      const option = el('option', '', entry.label);
      option.value = entry.key;
      option.selected = entry.key === key.key;
      select.append(option);
    }
    select.addEventListener('change', () => { state.trendKey = select.value; draw('[data-focus="trend"]'); });
    picker.append(select);
    box.append(picker);
    const points = trendPoints(file, key.key);
    const frame = { left: 60, right: 540, top: 16, bottom: 150 };
    const svg = plot(560, 176, `Trend of ${key.label}`);
    const max = niceMax(Math.max(...points.map((point) => point.value)));
    yAxis(svg, frame, max, key.unit);
    const xFor = (index) => (points.length < 2 ? (frame.left + frame.right) / 2 : frame.left + 14 + (index / (points.length - 1)) * (frame.right - frame.left - 28));
    const yFor = (value) => frame.bottom - (value / max) * (frame.bottom - frame.top);
    if (points.length > 1) svg.append(svgEl('polyline', { points: points.map((point, index) => `${xFor(index)},${yFor(point.value)}`).join(' '), stroke: 'var(--series-1)', class: 'bench-line' }));
    points.forEach((point, index) => {
      const here = point.id === file.id;
      const dot = svgEl('circle', { cx: xFor(index), cy: yFor(point.value), r: here ? 7 : 5, fill: 'var(--series-1)', class: `bench-dot${here ? ' current' : ''}` });
      attachTip(dot, tooltip, `${point.date} · v${point.appVersion}\n${key.label}: ${format(point.value, key.unit)}${here ? '\nThis report' : ''}`);
      svg.append(dot);
      const tick = svgEl('text', { x: xFor(index), y: frame.bottom + 16, class: 'bench-tick', 'text-anchor': 'middle' });
      tick.textContent = point.date;
      svg.append(tick);
    });
    box.append(svg);
    if (points.length < 2) box.append(el('p', 'bench-note', 'One report so far; the line grows with each published run.'));
    return box;
  }

  function examples(runs, ctx, feature) {
    const { file, state, draw } = ctx;
    const ids = new Set(runs.map((run) => run.id));
    const list = (file.examples || []).filter((example) => example.feature === feature && ids.has(example.runId));
    if (!list.length) return el('div');
    const box = el('section', 'bench-examples');
    box.setAttribute('aria-label', 'Example outputs');
    const head = el('div', 'bench-examples-head');
    head.append(el('h3', '', 'Example outputs'));
    const index = Math.min(state.example === null ? 0 : state.example, list.length - 1);
    const picker = el('label', 'bench-picker');
    picker.append(el('span', '', 'Setup'));
    const select = el('select');
    select.dataset.focus = 'example';
    list.forEach((example, position) => {
      const run = file.runs.find((entry) => entry.id === example.runId);
      const option = el('option', '', `${run ? describe(run) : example.runId} · ${example.sample}`);
      option.value = String(position);
      option.selected = position === index;
      select.append(option);
    });
    select.addEventListener('change', () => { state.example = Number(select.value); draw('[data-focus="example"]'); });
    picker.append(select);
    head.append(picker);
    box.append(head);
    const example = list[index];
    const run = file.runs.find((entry) => entry.id === example.runId);
    // Transcripts and OCR are compared with their reference; generated answers are shown as written.
    const reference = feature === 'dictation' || (feature === 'investigations' && example.sample.startsWith('ocr'));
    const diffable = reference || feature === 'formatting';
    const grid = el('div', 'bench-example-grid');
    const input = el('article', 'bench-example raw');
    input.append(el('small', '', feature === 'formatting' ? 'AS SPOKEN' : reference ? 'REFERENCE' : 'INPUT'), el('p', '', example.input));
    const output = el('article', 'bench-example');
    if (run && run.model) output.style.setProperty('--accent', colorOf(ctx, run.model));
    output.append(el('small', '', run ? describe(run).toUpperCase() : 'OUTPUT'));
    const text = el('p', 'bench-diff');
    if (diffable) {
      for (const part of wordDiff(example.input, example.output)) {
        const node = part.type === 'same' ? document.createTextNode(`${part.text} `) : el(part.type, '', `${part.text} `);
        if (part.type !== 'same') node.title = part.type === 'ins' ? 'Added' : 'Removed';
        text.append(node);
      }
    } else {
      text.textContent = example.output;
    }
    output.append(text);
    grid.append(input, output);
    box.append(grid);
    if (diffable) box.append(el('p', 'bench-note', 'Underlined words were added; struck-through words were removed or misheard.'));
    return box;
  }

  function table(runs, ctx) {
    const details = el('details', 'bench-table');
    details.append(el('summary', '', `Table view of ${runs.length} setups`));
    const names = unique(runs.flatMap((run) => Object.keys(run.metrics)));
    const grid = el('table');
    const head = el('tr');
    for (const name of ['Setup', 'Repeats', ...names.map((entry) => nameOf(ctx, entry))]) head.append(el('th', '', name));
    grid.append(head);
    for (const run of runs) {
      const line = el('tr');
      line.append(el('td', '', describe(run)), el('td', '', String(run.repeats)));
      for (const name of names) {
        const measure = run.metrics[name];
        line.append(el('td', '', measure ? `${format(measure.value, measure.unit)}${measure.p90 !== undefined && measure.p90 !== null ? ` (p90 ${format(measure.p90, measure.unit)})` : ''}` : ''));
      }
      grid.append(line);
    }
    const scroll = el('div', 'bench-table-scroll');
    scroll.append(grid);
    details.append(scroll);
    return details;
  }

  const api = { format, filterRuns, where, average, describe, wordDiff, trendPoints, modelOrder, measuredModels, render };
  global.PhorminxBenchmarks = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
