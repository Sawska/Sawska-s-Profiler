//! Self-contained HTML report with inline SVG graphs (no external assets, no
//! charting library). Matches the terminal's cyberpunk neon palette.

use super::Report;
use crate::interrupt::hardware_interrupt::{
    BulkMeasurement, FileMeasurement, SweepResult, LATENCY_BOUNDS_NS,
};
use std::time::{SystemTime, UNIX_EPOCH};

// ── small formatters ────────────────────────────────────────────────────────

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn human_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn human_ns(n: u64) -> String {
    if n < 1_000 {
        format!("{n} ns")
    } else if n < 1_000_000 {
        format!("{} µs", n / 1_000)
    } else {
        format!("{} ms", n / 1_000_000)
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// ── reusable SVG horizontal bar chart ───────────────────────────────────────

/// `rows` are (label, value, display-string). Bars scale to the max value.
fn bar_chart(title: &str, rows: &[(String, f64, String)], color: &str) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let max = rows.iter().map(|r| r.1).fold(0.0_f64, f64::max).max(1e-9);

    const ROW_H: usize = 30;
    const LABEL_W: usize = 250;
    const BAR_MAX: usize = 440;
    let width = 800;
    let height = rows.len() * ROW_H + 16;

    let mut svg = format!(
        "<svg viewBox=\"0 0 {width} {height}\" role=\"img\" preserveAspectRatio=\"xMinYMin meet\">"
    );
    for (i, (label, val, disp)) in rows.iter().enumerate() {
        let y = i * ROW_H + 8;
        let bar_y = y + 4;
        let w = ((val / max) * BAR_MAX as f64).max(2.0);
        svg.push_str(&format!(
            "<rect x=\"{LABEL_W}\" y=\"{bar_y}\" width=\"{w:.1}\" height=\"18\" rx=\"3\" \
fill=\"{color}\" opacity=\"0.85\"><title>{}</title></rect>",
            esc(disp)
        ));
        svg.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"end\" class=\"blab\">{}</text>",
            LABEL_W - 10,
            bar_y + 14,
            esc(label)
        ));
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{}\" class=\"bval\">{}</text>",
            LABEL_W as f64 + w + 8.0,
            bar_y + 14,
            esc(disp)
        ));
    }
    svg.push_str("</svg>");

    format!("<section class=\"chart\"><h2>{}</h2>{}</section>", esc(title), svg)
}

// ── cards ────────────────────────────────────────────────────────────────────

fn cards(items: &[(&str, String)]) -> String {
    let mut s = String::from("<div class=\"cards\">");
    for (label, value) in items {
        s.push_str(&format!(
            "<div class=\"card\"><div class=\"clab\">{}</div><div class=\"cval\">{}</div></div>",
            esc(label),
            esc(value)
        ));
    }
    s.push_str("</div>");
    s
}

// ── profile body ─────────────────────────────────────────────────────────────

fn profile_body(root: &str, files: &[FileMeasurement]) -> String {
    let total_bytes: u64 = files.iter().map(|f| f.bytes_read).sum();
    let total_nanos: u64 = files.iter().map(|f| f.wall_nanos).sum();
    let agg_mibps = if total_nanos > 0 {
        (total_bytes as f64 / (1024.0 * 1024.0)) / (total_nanos as f64 / 1e9)
    } else {
        0.0
    };

    let uncached = files.first().map(|f| f.uncached).unwrap_or(false);
    let mut body = cards(&[
        ("FILES", files.len().to_string()),
        ("TOTAL SIZE", human_bytes(total_bytes)),
        ("AGG THROUGHPUT", format!("{agg_mibps:.1} MiB/s")),
        ("READ TIME", format!("{:.2} ms", total_nanos as f64 / 1e6)),
        ("READ MODE", if uncached { "uncached".into() } else { "cached".into() }),
    ]);

    // Throughput per file — slowest first (bottlenecks on top), capped.
    let mut tput: Vec<&FileMeasurement> = files.iter().collect();
    tput.sort_by(|a, b| {
        a.throughput_mib_s()
            .partial_cmp(&b.throughput_mib_s())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let tput_rows: Vec<(String, f64, String)> = tput
        .iter()
        .take(30)
        .map(|m| {
            (
                basename(&m.path).to_string(),
                m.throughput_mib_s(),
                format!("{:.1} MiB/s", m.throughput_mib_s()),
            )
        })
        .collect();
    body.push_str(&bar_chart(
        "Read throughput per file (slowest first)",
        &tput_rows,
        "#00ff8c",
    ));

    // Aggregate latency histogram across all files.
    let mut agg_buckets = [0u64; LATENCY_BOUNDS_NS.len() + 1];
    for m in files {
        for (i, c) in m.latency_buckets.iter().enumerate() {
            agg_buckets[i] += c;
        }
    }
    let hist_rows: Vec<(String, f64, String)> = agg_buckets
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(i, c)| {
            let c = *c;
            let label = if i < LATENCY_BOUNDS_NS.len() {
                format!("≤ {}", human_ns(LATENCY_BOUNDS_NS[i]))
            } else {
                format!("> {}", human_ns(LATENCY_BOUNDS_NS[LATENCY_BOUNDS_NS.len() - 1]))
            };
            (label, c as f64, c.to_string())
        })
        .collect();
    body.push_str(&bar_chart(
        "Read-latency distribution (chunks)",
        &hist_rows,
        "#00f7ff",
    ));

    // Detail table.
    let mut table = String::from(
        "<section class=\"chart\"><h2>Files</h2><table><thead><tr>\
<th>path</th><th class=\"r\">size</th><th class=\"r\">throughput</th>\
<th class=\"r\">avg</th><th class=\"r\">p99</th><th class=\"r\">chunks</th>\
</tr></thead><tbody>",
    );
    for m in files {
        let p99 = m.percentiles_ns(&[99.0])[0];
        table.push_str(&format!(
            "<tr><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{:.1} MiB/s</td>\
<td class=\"r\">{} ns</td><td class=\"r\">{} ns</td><td class=\"r\">{}</td></tr>",
            esc(&m.path),
            human_bytes(m.size_bytes),
            m.throughput_mib_s(),
            m.avg_chunk_nanos(),
            p99,
            m.chunk_count,
        ));
    }
    table.push_str("</tbody></table></section>");
    body.push_str(&table);

    let _ = root;
    body
}

// ── bulk body ────────────────────────────────────────────────────────────────

fn bulk_body(root: &str, bulk: &BulkMeasurement) -> String {
    let mut body = cards(&[
        ("THREADS", bulk.threads.to_string()),
        ("FILES", format!("{} · {} failed", bulk.file_count, bulk.errors)),
        ("TOTAL READ", human_bytes(bulk.bytes_read)),
        ("THROUGHPUT", format!("{:.1} MiB/s", bulk.throughput_mib_s())),
        ("READ MODE", if bulk.uncached { "uncached".into() } else { "cached".into() }),
    ]);

    // Bytes processed per worker thread — the "threads" graph.
    let bytes_rows: Vec<(String, f64, String)> = bulk
        .per_thread
        .iter()
        .map(|t| {
            (
                format!("thread {} ({} files)", t.index, t.files),
                t.bytes as f64,
                human_bytes(t.bytes),
            )
        })
        .collect();
    body.push_str(&bar_chart(
        "Bytes read per worker thread",
        &bytes_rows,
        "#ff00c7",
    ));

    // Throughput per worker thread.
    let tput_rows: Vec<(String, f64, String)> = bulk
        .per_thread
        .iter()
        .map(|t| {
            (
                format!("thread {}", t.index),
                t.throughput_mib_s(),
                format!("{:.1} MiB/s", t.throughput_mib_s()),
            )
        })
        .collect();
    body.push_str(&bar_chart(
        "Throughput per worker thread",
        &tput_rows,
        "#00ff8c",
    ));

    let _ = root;
    body
}

// ── sweep body ───────────────────────────────────────────────────────────────

fn sweep_body(sweep: &SweepResult) -> String {
    let best = sweep.best();
    let best_str = best
        .map(|p| format!("{} → {:.0} MiB/s", human_bytes(p.chunk_size as u64), p.throughput_mib_s()))
        .unwrap_or_else(|| "—".to_string());

    let mut body = cards(&[
        ("FILE SIZE", human_bytes(sweep.size_bytes)),
        ("BLOCK SIZES", sweep.points.len().to_string()),
        ("BEST BLOCK", best_str),
        ("READ MODE", if sweep.uncached { "uncached".into() } else { "cached".into() }),
    ]);

    let tput_rows: Vec<(String, f64, String)> = sweep
        .points
        .iter()
        .map(|p| {
            (
                human_bytes(p.chunk_size as u64),
                p.throughput_mib_s(),
                format!("{:.0} MiB/s", p.throughput_mib_s()),
            )
        })
        .collect();
    body.push_str(&bar_chart(
        "Read throughput by block size",
        &tput_rows,
        "#00f7ff",
    ));

    let lat_rows: Vec<(String, f64, String)> = sweep
        .points
        .iter()
        .map(|p| {
            (
                human_bytes(p.chunk_size as u64),
                p.avg_chunk_nanos as f64,
                human_ns(p.avg_chunk_nanos),
            )
        })
        .collect();
    body.push_str(&bar_chart(
        "Average read() latency by block size",
        &lat_rows,
        "#a200ff",
    ));

    body
}

// ── page shell ───────────────────────────────────────────────────────────────

pub fn render(r: &Report) -> String {
    let (kind, root, body) = match r {
        Report::Profile { root, files } => ("Profile", root.clone(), profile_body(root, files)),
        Report::Bulk { root, bulk } => ("Bulk throughput", root.clone(), bulk_body(root, bulk)),
        Report::Sweep { sweep } => ("Block-size sweep", sweep.path.clone(), sweep_body(sweep)),
    };

    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>SAWSKAS PROFILER · {kind}</title>
<style>
:root {{ --bg:#0a0e14; --panel:#11161f; --line:#1d2735; --text:#c8d3e0; --dim:#58607c;
  --green:#00ff8c; --cyan:#00f7ff; --magenta:#ff00c7; --yellow:#fcee09; }}
* {{ box-sizing:border-box; }}
body {{ margin:0; background:var(--bg); color:var(--text);
  font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; padding:28px; }}
header h1 {{ margin:0; font-size:22px; letter-spacing:2px;
  background:linear-gradient(90deg,var(--cyan),var(--magenta));
  -webkit-background-clip:text; background-clip:text; color:transparent; }}
header .sub {{ color:var(--dim); margin-top:4px; }}
header .sub b {{ color:var(--yellow); font-weight:600; }}
.cards {{ display:flex; flex-wrap:wrap; gap:14px; margin:22px 0; }}
.card {{ background:var(--panel); border:1px solid var(--line); border-radius:10px;
  padding:14px 18px; min-width:150px; flex:1; }}
.card .clab {{ color:var(--dim); font-size:11px; letter-spacing:1px; }}
.card .cval {{ color:var(--green); font-size:20px; font-weight:600; margin-top:4px; }}
.chart {{ background:var(--panel); border:1px solid var(--line); border-radius:10px;
  padding:16px 18px; margin:18px 0; overflow-x:auto; }}
.chart h2 {{ margin:0 0 12px; font-size:13px; letter-spacing:1px; color:var(--cyan);
  text-transform:uppercase; }}
svg {{ width:100%; height:auto; }}
.blab {{ fill:var(--text); font-size:12px; }}
.bval {{ fill:var(--dim); font-size:11px; }}
table {{ width:100%; border-collapse:collapse; font-size:12px; }}
th,td {{ text-align:left; padding:6px 10px; border-bottom:1px solid var(--line); }}
th {{ color:var(--dim); font-weight:600; }}
td {{ color:var(--text); }}
.r {{ text-align:right; }}
footer {{ color:var(--dim); margin-top:24px; font-size:11px; }}
</style></head>
<body>
<header>
  <h1>SAWSKAS PROFILER</h1>
  <div class="sub">{kind} report · target <b>{root}</b> · <span id="gen"></span></div>
</header>
{body}
<footer>Generated by Sawskas Profiler — hardware-interrupt file measurement.</footer>
<script>
  document.getElementById('gen').textContent =
    'generated ' + new Date({generated}000).toLocaleString();
</script>
</body></html>
"#,
        kind = esc(kind),
        root = esc(&root),
        body = body,
        generated = generated,
    )
}
