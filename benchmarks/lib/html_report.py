"""Standalone interactive HTML renderer for Casita benchmark results."""

from __future__ import annotations

import html
import json
from typing import Any


def render_html(result: dict[str, Any]) -> str:
    environment = result["environment"]
    configuration = result.get("configuration", {})
    tools = result["tools"]
    samples = result["samples"]
    aggregates = result["aggregates"]
    successful = sum(sample["status"] == "ok" for sample in samples)
    total_bytes = sum(int(sample.get("source_bytes", 0)) for sample in samples if sample["status"] == "ok")
    dirty = bool(environment.get("casita_worktree_dirty"))
    status_label = "Development baseline" if dirty else "Release baseline"
    status_class = "development" if dirty else "release"
    revision = str(environment.get("casita_revision") or "unknown")[:12]
    captured = str(environment.get("captured_at_utc") or "unknown")
    repetitions = configuration.get("repetitions", "?")
    data = json.dumps(result, separators=(",", ":"), ensure_ascii=False).replace("</", "<\\/")

    tool_cards = "".join(
        f"""
        <article class="tool-card" data-tool="{html.escape(name)}">
          <span class="tool-dot"></span>
          <div><strong>{html.escape(name)}</strong><small>{html.escape(str(info.get('version') or info.get('reason') or 'unknown'))}</small></div>
        </article>
        """
        for name, info in tools.items()
    )

    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="Repeatable end-to-end performance measurements for Casita, Git, restic, Borg, and tar+zstd.">
  <title>Casita benchmarks — performance in context</title>
  <style>
    :root {{
      --ink: #16201c;
      --muted: #617069;
      --paper: #f5f1e8;
      --card: rgba(255,255,255,.82);
      --line: #d8d2c5;
      --lime: #c9f27b;
      --green: #164c3b;
      --orange: #ff8a4c;
      --blue: #5e7bea;
      --shadow: 0 22px 70px rgba(24, 39, 32, .10);
      color-scheme: light;
    }}
    * {{ box-sizing: border-box; }}
    html {{ scroll-behavior: smooth; }}
    body {{
      margin: 0;
      color: var(--ink);
      background:
        radial-gradient(circle at 82% 0%, rgba(201,242,123,.48), transparent 28rem),
        radial-gradient(circle at 0% 34%, rgba(94,123,234,.10), transparent 30rem),
        var(--paper);
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      font-variant-numeric: tabular-nums;
    }}
    a {{ color: inherit; }}
    button, select {{ font: inherit; }}
    .shell {{ width: min(1180px, calc(100% - 40px)); margin: 0 auto; }}
    nav {{ display: flex; align-items: center; justify-content: space-between; padding: 24px 0; }}
    .brand {{ display: inline-flex; gap: 10px; align-items: center; font-weight: 800; letter-spacing: -.03em; text-decoration: none; }}
    .brand-mark {{ width: 30px; height: 30px; border-radius: 9px; background: var(--ink); position: relative; box-shadow: inset 0 0 0 1px rgba(255,255,255,.16); }}
    .brand-mark::before, .brand-mark::after {{ content: ""; position: absolute; background: var(--lime); border-radius: 999px; }}
    .brand-mark::before {{ width: 14px; height: 5px; left: 8px; top: 8px; }}
    .brand-mark::after {{ width: 5px; height: 14px; left: 8px; top: 8px; }}
    .nav-link {{ font-size: .88rem; color: var(--muted); text-decoration-thickness: 1px; text-underline-offset: 4px; }}
    header {{ padding: 70px 0 54px; }}
    .eyebrow {{ display: inline-flex; align-items: center; gap: 8px; margin-bottom: 24px; color: var(--green); font-size: .76rem; font-weight: 800; letter-spacing: .13em; text-transform: uppercase; }}
    .eyebrow::before {{ content: ""; width: 28px; height: 2px; background: currentColor; }}
    h1 {{ max-width: 900px; margin: 0; font-family: Georgia, "Times New Roman", serif; font-size: clamp(3.4rem, 8vw, 7.3rem); font-weight: 500; letter-spacing: -.065em; line-height: .88; }}
    h1 em {{ color: var(--green); font-weight: 500; }}
    .lede {{ max-width: 720px; margin: 34px 0 0; color: var(--muted); font-size: clamp(1.05rem, 2vw, 1.3rem); line-height: 1.65; }}
    .status-row {{ display: flex; flex-wrap: wrap; gap: 10px; margin-top: 28px; }}
    .pill {{ display: inline-flex; align-items: center; gap: 7px; padding: 8px 12px; border: 1px solid var(--line); border-radius: 999px; background: rgba(255,255,255,.45); color: var(--muted); font-size: .78rem; }}
    .pill::before {{ content: ""; width: 7px; height: 7px; border-radius: 50%; background: var(--green); }}
    .pill.development::before {{ background: var(--orange); }}
    .metrics {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 1px; overflow: hidden; margin: 14px 0 86px; border: 1px solid var(--line); border-radius: 20px; background: var(--line); box-shadow: var(--shadow); }}
    .metric {{ padding: 28px; background: rgba(255,255,255,.88); }}
    .metric strong {{ display: block; font-family: Georgia, serif; font-size: clamp(2rem, 4vw, 3.2rem); font-weight: 500; letter-spacing: -.045em; }}
    .metric span {{ color: var(--muted); font-size: .8rem; letter-spacing: .06em; text-transform: uppercase; }}
    section {{ margin: 0 0 88px; }}
    .section-head {{ display: flex; align-items: end; justify-content: space-between; gap: 30px; margin-bottom: 24px; }}
    h2 {{ margin: 0; font-family: Georgia, serif; font-size: clamp(2rem, 5vw, 3.6rem); font-weight: 500; letter-spacing: -.045em; }}
    .section-note {{ max-width: 510px; margin: 0; color: var(--muted); line-height: 1.55; }}
    .explorer {{ overflow: hidden; border: 1px solid var(--line); border-radius: 24px; background: var(--card); box-shadow: var(--shadow); backdrop-filter: blur(12px); }}
    .controls {{ display: grid; grid-template-columns: repeat(3, 1fr); gap: 16px; padding: 22px; border-bottom: 1px solid var(--line); background: rgba(255,255,255,.42); }}
    label {{ display: grid; gap: 7px; color: var(--muted); font-size: .72rem; font-weight: 800; letter-spacing: .09em; text-transform: uppercase; }}
    select {{ width: 100%; appearance: none; padding: 12px 38px 12px 13px; color: var(--ink); border: 1px solid var(--line); border-radius: 10px; background: #fff url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='8'%3E%3Cpath d='m1 1 5 5 5-5' fill='none' stroke='%23617069' stroke-width='1.5'/%3E%3C/svg%3E") no-repeat right 13px center; }}
    .chart {{ min-height: 350px; padding: 30px 28px 34px; }}
    .chart-title {{ display: flex; justify-content: space-between; gap: 16px; margin-bottom: 28px; }}
    .chart-title strong {{ font-size: 1.15rem; }}
    .chart-title span {{ color: var(--muted); font-size: .8rem; }}
    .bar-row {{ display: grid; grid-template-columns: 110px 1fr 115px; align-items: center; gap: 18px; margin: 16px 0; }}
    .bar-label {{ font-weight: 760; }}
    .bar-track {{ height: 34px; overflow: hidden; border-radius: 8px; background: #e9e5dc; }}
    .bar {{ width: var(--width); min-width: 3px; height: 100%; border-radius: inherit; background: var(--bar); transform-origin: left; animation: grow .55s cubic-bezier(.2,.8,.2,1) both; }}
    .bar-value {{ text-align: right; font-weight: 700; }}
    .bar-value small {{ display: block; color: var(--muted); font-size: .68rem; font-weight: 500; }}
    .empty {{ display: grid; place-items: center; min-height: 260px; color: var(--muted); }}
    @keyframes grow {{ from {{ transform: scaleX(0); }} to {{ transform: scaleX(1); }} }}
    .tools {{ display: grid; grid-template-columns: repeat(5, 1fr); gap: 12px; }}
    .tool-card {{ display: flex; min-width: 0; gap: 11px; align-items: flex-start; padding: 18px; border: 1px solid var(--line); border-radius: 14px; background: rgba(255,255,255,.62); }}
    .tool-card strong, .tool-card small {{ display: block; }}
    .tool-card small {{ overflow: hidden; margin-top: 5px; color: var(--muted); font-size: .69rem; line-height: 1.35; text-overflow: ellipsis; }}
    .tool-dot {{ flex: 0 0 auto; width: 9px; height: 9px; margin-top: 5px; border-radius: 50%; background: var(--tool-color); }}
    [data-tool="casita"] {{ --tool-color: #164c3b; }} [data-tool="git"] {{ --tool-color: #f05133; }} [data-tool="tar-zstd"] {{ --tool-color: #5e7bea; }} [data-tool="restic"] {{ --tool-color: #65a8d8; }} [data-tool="borg"] {{ --tool-color: #75599b; }}
    .method-grid {{ display: grid; grid-template-columns: 1.25fr .75fr; gap: 18px; }}
    .method-card {{ padding: 28px; border: 1px solid var(--line); border-radius: 18px; background: rgba(255,255,255,.58); }}
    .method-card h3 {{ margin: 0 0 12px; font-size: 1rem; }}
    .method-card p, .method-card li {{ color: var(--muted); line-height: 1.65; }}
    .method-card ul {{ margin: 0; padding-left: 20px; }}
    .environment {{ display: grid; grid-template-columns: repeat(2, 1fr); gap: 8px 30px; margin: 0; }}
    .environment div {{ display: flex; justify-content: space-between; gap: 20px; padding: 9px 0; border-bottom: 1px solid var(--line); }}
    .environment dt {{ color: var(--muted); }} .environment dd {{ margin: 0; text-align: right; font-weight: 650; }}
    .actions {{ display: flex; flex-wrap: wrap; gap: 10px; margin-top: 20px; }}
    .button {{ cursor: pointer; padding: 11px 15px; border: 1px solid var(--ink); border-radius: 10px; background: var(--ink); color: #fff; font-weight: 700; }}
    .button.secondary {{ background: transparent; color: var(--ink); }}
    footer {{ display: flex; justify-content: space-between; gap: 24px; padding: 30px 0 50px; border-top: 1px solid var(--line); color: var(--muted); font-size: .8rem; }}
    @media (max-width: 840px) {{
      header {{ padding-top: 40px; }} .metrics {{ grid-template-columns: repeat(2, 1fr); }} .tools {{ grid-template-columns: repeat(2, 1fr); }} .method-grid {{ grid-template-columns: 1fr; }} .section-head {{ display: block; }} .section-note {{ margin-top: 12px; }}
    }}
    @media (max-width: 560px) {{
      .shell {{ width: min(100% - 24px, 1180px); }} .controls {{ grid-template-columns: 1fr; }} .bar-row {{ grid-template-columns: 78px 1fr; gap: 10px; }} .bar-value {{ grid-column: 2; }} .metrics {{ grid-template-columns: 1fr 1fr; }} .metric {{ padding: 20px; }} .tools {{ grid-template-columns: 1fr; }} .environment {{ grid-template-columns: 1fr; }} footer {{ display: block; }}
    }}
    @media (prefers-reduced-motion: reduce) {{ * {{ scroll-behavior: auto !important; animation: none !important; }} }}
  </style>
</head>
<body>
  <nav class="shell">
    <a class="brand" href="/"><span class="brand-mark" aria-hidden="true"></span>casita</a>
    <a class="nav-link" href="https://github.com/cachix/casita/tree/main/benchmarks">Methodology &amp; raw suite ↗</a>
  </nav>
  <main class="shell">
    <header>
      <div class="eyebrow">End-to-end benchmark report</div>
      <h1>Performance,<br><em>in context.</em></h1>
      <p class="lede">Repeatable measurements of complete repository workflows—not isolated happy paths—across Casita and tools with overlapping jobs.</p>
      <div class="status-row">
        <span class="pill {status_class}">{html.escape(status_label)}</span>
        <span class="pill">revision {html.escape(revision)}</span>
        <span class="pill">{html.escape(str(repetitions))}× balanced repetitions</span>
      </div>
    </header>

    <div class="metrics" aria-label="Run summary">
      <div class="metric"><strong>{len(samples):,}</strong><span>raw samples</span></div>
      <div class="metric"><strong>{successful:,}</strong><span>validated runs</span></div>
      <div class="metric"><strong>{len(aggregates):,}</strong><span>scenarios</span></div>
      <div class="metric"><strong>{_human_bytes(total_bytes)}</strong><span>logical bytes processed</span></div>
    </div>

    <section>
      <div class="section-head">
        <h2>Explore the run</h2>
        <p class="section-note">Lower latency is better. Every bar is the median of fresh, validated repositories; p95 exposes tail behavior.</p>
      </div>
      <div class="explorer">
        <div class="controls">
          <label>Corpus<select id="corpus"></select></label>
          <label>Cache policy<select id="cache"></select></label>
          <label>Operation<select id="operation"></select></label>
        </div>
        <div class="chart" id="chart" aria-live="polite"></div>
      </div>
    </section>

    <section>
      <div class="section-head"><h2>Compared tools</h2><p class="section-note">Versions are captured with the run. Workflows overlap, but guarantees do not.</p></div>
      <div class="tools">{tool_cards}</div>
    </section>

    <section>
      <div class="section-head"><h2>Read this honestly</h2><p class="section-note">The report makes differences visible instead of compressing unlike systems into one league table.</p></div>
      <div class="method-grid">
        <article class="method-card">
          <h3>What the harness controls</h3>
          <ul>
            <li>Deterministic small-file, mixed, and large-file corpora.</li>
            <li>Fresh repository state for every sample and balanced tool ordering.</li>
            <li>Warm file-data reads or POSIX cache-eviction hints, recorded separately.</li>
            <li>Post-run integrity checks and byte-, mode-, path-, and symlink-exact restores.</li>
          </ul>
        </article>
        <article class="method-card">
          <h3>Guarantees differ</h3>
          <p>Casita verifies object identity and graph closure before publication. Git models versioned source trees. Restic and Borg model backup snapshots. tar+zstd is a one-shot archive baseline.</p>
          <div class="actions"><button class="button" id="download">Download raw JSON</button><a class="button secondary" href="https://github.com/cachix/casita/blob/main/benchmarks/README.md">Full method</a></div>
        </article>
      </div>
    </section>

    <section>
      <div class="section-head"><h2>Run environment</h2><p class="section-note">Performance belongs to a machine, filesystem, build, and toolchain. These are part of the result.</p></div>
      <article class="method-card">
        <dl class="environment">
          <div><dt>CPU</dt><dd>{html.escape(str(environment.get('cpu') or 'unknown'))}</dd></div>
          <div><dt>Architecture</dt><dd>{html.escape(str(environment.get('architecture') or 'unknown'))}</dd></div>
          <div><dt>Kernel</dt><dd>{html.escape(str(environment.get('kernel') or 'unknown'))}</dd></div>
          <div><dt>Filesystem</dt><dd>{html.escape(str(environment.get('filesystem') or 'unknown'))}</dd></div>
          <div><dt>CPU governor</dt><dd>{html.escape(str(environment.get('cpu_governor') or 'unknown'))}</dd></div>
          <div><dt>Captured</dt><dd>{html.escape(captured)}</dd></div>
        </dl>
      </article>
    </section>
  </main>
  <footer class="shell"><span>Casita benchmark schema v{result['schema_version']}</span><span>Raw samples are authoritative · p95 uses nearest rank</span></footer>

  <script id="benchmark-data" type="application/json">{data}</script>
  <script>
    const data = JSON.parse(document.getElementById('benchmark-data').textContent);
    const rows = data.aggregates;
    const colors = {{casita:'#164c3b',git:'#f05133','tar-zstd':'#5e7bea',restic:'#65a8d8',borg:'#75599b'}};
    const corpus = document.getElementById('corpus');
    const cache = document.getElementById('cache');
    const operation = document.getElementById('operation');
    const unique = (key) => [...new Set(rows.map(row => row[key]))];
    const fill = (node, values, preferred) => {{
      node.replaceChildren(...values.map(value => Object.assign(document.createElement('option'), {{value, textContent: value}})));
      if (values.includes(preferred)) node.value = preferred;
    }};
    fill(corpus, unique('corpus'), 'mixed');
    fill(cache, unique('cache_policy'), 'warm');
    fill(operation, unique('operation'), 'cold-import');
    const seconds = value => value < .001 ? `${{(value * 1e6).toFixed(0)}} µs` : value < 1 ? `${{(value * 1e3).toFixed(1)}} ms` : `${{value.toFixed(2)}} s`;
    const bytes = value => {{ let unit = 0; const units = ['B','KiB','MiB','GiB']; while (value >= 1024 && unit < units.length - 1) {{ value /= 1024; unit++; }} return `${{value.toFixed(1)}} ${{units[unit]}}`; }};
    function render() {{
      const selected = rows.filter(row => row.corpus === corpus.value && row.cache_policy === cache.value && row.operation === operation.value).sort((a,b) => a.median_wall_seconds - b.median_wall_seconds);
      const chart = document.getElementById('chart');
      if (!selected.length) {{ chart.innerHTML = '<div class="empty">No implementation supports this combination.</div>'; return; }}
      const max = Math.max(...selected.map(row => row.median_wall_seconds));
      chart.innerHTML = `<div class="chart-title"><strong>${{operation.value}} · ${{corpus.value}}</strong><span>${{cache.value}} file-data cache · median / p95</span></div>` + selected.map(row => `
        <div class="bar-row">
          <div class="bar-label">${{row.implementation}}</div>
          <div class="bar-track"><div class="bar" style="--width:${{Math.max(1.5, row.median_wall_seconds / max * 100)}}%;--bar:${{colors[row.implementation]}}"></div></div>
          <div class="bar-value">${{seconds(row.median_wall_seconds)}}<small>p95 ${{seconds(row.p95_wall_seconds)}} · ${{bytes(row.median_max_rss_bytes)}} RSS</small></div>
        </div>`).join('');
    }}
    [corpus, cache, operation].forEach(node => node.addEventListener('change', render));
    document.getElementById('download').addEventListener('click', () => {{
      const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2) + '\\n'], {{type:'application/json'}}));
      const link = Object.assign(document.createElement('a'), {{href:url, download:'casita-benchmark.json'}});
      link.click(); URL.revokeObjectURL(url);
    }});
    render();
  </script>
</body>
</html>
"""


def _human_bytes(value: int) -> str:
    number = float(value)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if number < 1024 or unit == "TiB":
            return f"{number:.1f} {unit}"
        number /= 1024
    raise AssertionError
