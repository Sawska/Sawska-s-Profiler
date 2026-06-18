//! Hand-rolled JSON serialization (no serde).

use super::Report;
use crate::interrupt::hardware_interrupt::{FileMeasurement, LATENCY_BOUNDS_NS};

/// Escapes a string for inclusion in a JSON double-quoted literal.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn file_object(m: &FileMeasurement) -> String {
    // Latency histogram as [{max_ns, count}, …]; the overflow bucket has max_ns: null.
    let mut buckets = String::new();
    for (i, count) in m.latency_buckets.iter().enumerate() {
        if i > 0 {
            buckets.push(',');
        }
        let bound = if i < LATENCY_BOUNDS_NS.len() {
            LATENCY_BOUNDS_NS[i].to_string()
        } else {
            "null".to_string()
        };
        buckets.push_str(&format!("{{\"max_ns\":{bound},\"count\":{count}}}"));
    }

    let pct = m.percentiles_ns(&[50.0, 95.0, 99.0]);
    format!(
        "{{\"path\":\"{}\",\"size_bytes\":{},\"bytes_read\":{},\"wall_nanos\":{},\
\"throughput_mib_s\":{:.4},\"uncached\":{},\"chunk_count\":{},\"avg_chunk_ns\":{},\
\"min_chunk_ns\":{},\"max_chunk_ns\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\
\"read_busy_fraction\":{:.4},\"timer_interrupts\":{},\"counter_hz\":{},\
\"samples\":{},\"latency_histogram\":[{}]}}",
        esc(&m.path),
        m.size_bytes,
        m.bytes_read,
        m.wall_nanos,
        m.throughput_mib_s(),
        m.uncached,
        m.chunk_count,
        m.avg_chunk_nanos(),
        m.min_chunk_nanos(),
        m.max_chunk_nanos(),
        pct[0],
        pct[1],
        pct[2],
        m.read_busy_fraction(),
        m.timer_interrupts,
        m.counter_hz,
        m.samples.len(),
        buckets,
    )
}

pub fn render(r: &Report) -> String {
    match r {
        Report::Profile { root, files } => {
            let total_bytes: u64 = files.iter().map(|f| f.bytes_read).sum();
            let objs: Vec<String> = files.iter().map(file_object).collect();
            format!(
                "{{\"kind\":\"profile\",\"root\":\"{}\",\"file_count\":{},\
\"total_bytes\":{},\"files\":[{}]}}\n",
                esc(root),
                files.len(),
                total_bytes,
                objs.join(",")
            )
        }
        Report::Bulk { root, bulk } => {
            let threads: Vec<String> = bulk
                .per_thread
                .iter()
                .map(|t| {
                    format!(
                        "{{\"index\":{},\"files\":{},\"bytes\":{},\"wall_nanos\":{},\
\"throughput_mib_s\":{:.4}}}",
                        t.index,
                        t.files,
                        t.bytes,
                        t.wall_nanos,
                        t.throughput_mib_s()
                    )
                })
                .collect();
            format!(
                "{{\"kind\":\"bulk\",\"root\":\"{}\",\"threads\":{},\"uncached\":{},\
\"file_count\":{},\"bytes_read\":{},\"wall_nanos\":{},\"throughput_mib_s\":{:.4},\
\"errors\":{},\"counter_hz\":{},\"counter_ticks\":{},\"per_thread\":[{}]}}\n",
                esc(root),
                bulk.threads,
                bulk.uncached,
                bulk.file_count,
                bulk.bytes_read,
                bulk.wall_nanos,
                bulk.throughput_mib_s(),
                bulk.errors,
                bulk.counter_hz,
                bulk.counter_ticks,
                threads.join(",")
            )
        }
        Report::Sweep { sweep } => {
            let points: Vec<String> = sweep
                .points
                .iter()
                .map(|p| {
                    format!(
                        "{{\"chunk_size\":{},\"bytes\":{},\"chunks\":{},\"wall_nanos\":{},\
\"avg_chunk_ns\":{},\"throughput_mib_s\":{:.4}}}",
                        p.chunk_size,
                        p.bytes,
                        p.chunks,
                        p.wall_nanos,
                        p.avg_chunk_nanos,
                        p.throughput_mib_s()
                    )
                })
                .collect();
            let best = sweep.best().map(|p| p.chunk_size.to_string()).unwrap_or_else(|| "null".into());
            format!(
                "{{\"kind\":\"sweep\",\"path\":\"{}\",\"size_bytes\":{},\"counter_hz\":{},\
\"uncached\":{},\"best_chunk_size\":{},\"points\":[{}]}}\n",
                esc(&sweep.path),
                sweep.size_bytes,
                sweep.counter_hz,
                sweep.uncached,
                best,
                points.join(",")
            )
        }
        Report::Write { write } => format!(
            "{{\"kind\":\"write\",\"path\":\"{}\",\"bytes\":{},\"chunk_size\":{},\
\"wall_nanos\":{},\"fsync_nanos\":{},\"throughput_mib_s\":{:.4},\"counter_hz\":{}}}\n",
            esc(&write.path),
            write.bytes,
            write.chunk_size,
            write.wall_nanos,
            write.fsync_nanos,
            write.throughput_mib_s(),
            write.counter_hz,
        ),
        Report::Random { random } => format!(
            "{{\"kind\":\"random\",\"path\":\"{}\",\"block_size\":{},\"ops\":{},\"bytes\":{},\
\"wall_nanos\":{},\"iops\":{:.1},\"avg_ns\":{},\"p99_ns\":{},\"uncached\":{},\"counter_hz\":{}}}\n",
            esc(&random.path),
            random.block_size,
            random.ops,
            random.bytes,
            random.wall_nanos,
            random.iops(),
            random.avg_nanos,
            random.p99_nanos,
            random.uncached,
            random.counter_hz,
        ),
    }
}
