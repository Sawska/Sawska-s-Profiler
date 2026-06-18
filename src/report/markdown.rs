//! Markdown report rendering — a readable summary with GFM tables.

use super::{ext_breakdown, Report};

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

pub fn render(r: &Report) -> String {
    let mut s = String::new();
    match r {
        Report::Profile { root, files } => {
            let total: u64 = files.iter().map(|f| f.bytes_read).sum();
            let nanos: u64 = files.iter().map(|f| f.wall_nanos).sum();
            let mibps = if nanos > 0 {
                (total as f64 / (1024.0 * 1024.0)) / (nanos as f64 / 1e9)
            } else {
                0.0
            };
            let mode = if files.first().map(|f| f.uncached).unwrap_or(false) {
                "uncached"
            } else {
                "cached"
            };
            s.push_str("# Sawskas Profiler — profile report\n\n");
            s.push_str(&format!(
                "**Target:** `{root}` · **Files:** {} · **Total:** {} · **Throughput:** {mibps:.1} MiB/s · **Mode:** {mode}\n\n",
                files.len(),
                human_bytes(total),
            ));
            s.push_str("| path | size | throughput | avg | p99 |\n|---|--:|--:|--:|--:|\n");
            for m in files {
                let p99 = m.percentiles_ns(&[99.0])[0];
                s.push_str(&format!(
                    "| `{}` | {} | {:.1} MiB/s | {} ns | {} ns |\n",
                    m.path,
                    human_bytes(m.size_bytes),
                    m.throughput_mib_s(),
                    m.avg_chunk_nanos(),
                    p99,
                ));
            }
            // Breakdown by file type.
            let by_ext = ext_breakdown(files);
            if by_ext.len() > 1 {
                s.push_str("\n## By type\n\n| ext | files | size | throughput |\n|---|--:|--:|--:|\n");
                for e in &by_ext {
                    s.push_str(&format!(
                        "| {} | {} | {} | {:.1} MiB/s |\n",
                        e.ext,
                        e.files,
                        human_bytes(e.bytes),
                        e.throughput_mib_s(),
                    ));
                }
            }
        }
        Report::Bulk { root, bulk } => {
            s.push_str("# Sawskas Profiler — bulk throughput report\n\n");
            s.push_str(&format!(
                "**Target:** `{root}` · **Threads:** {} · **Files:** {} · **Total:** {} · **Throughput:** {:.1} MiB/s\n\n",
                bulk.threads,
                bulk.file_count,
                human_bytes(bulk.bytes_read),
                bulk.throughput_mib_s(),
            ));
            s.push_str("| thread | files | bytes | throughput |\n|--:|--:|--:|--:|\n");
            for t in &bulk.per_thread {
                s.push_str(&format!(
                    "| {} | {} | {} | {:.1} MiB/s |\n",
                    t.index,
                    t.files,
                    human_bytes(t.bytes),
                    t.throughput_mib_s(),
                ));
            }
        }
        Report::Sweep { sweep } => {
            s.push_str("# Sawskas Profiler — block-size sweep\n\n");
            s.push_str(&format!(
                "**File:** `{}` · **Size:** {} · **Mode:** {}\n\n",
                sweep.path,
                human_bytes(sweep.size_bytes),
                if sweep.uncached { "uncached" } else { "cached" },
            ));
            s.push_str("| block size | throughput | avg latency |\n|---|--:|--:|\n");
            for p in &sweep.points {
                s.push_str(&format!(
                    "| {} | {:.1} MiB/s | {} ns |\n",
                    human_bytes(p.chunk_size as u64),
                    p.throughput_mib_s(),
                    p.avg_chunk_nanos,
                ));
            }
            if let Some(b) = sweep.best() {
                s.push_str(&format!(
                    "\n**Best block size:** {}\n",
                    human_bytes(b.chunk_size as u64)
                ));
            }
        }
        Report::Write { write } => {
            s.push_str("# Sawskas Profiler — write benchmark\n\n");
            s.push_str(&format!(
                "- **Bytes written:** {}\n- **Throughput:** {:.1} MiB/s\n- **Block size:** {}\n- **Wall time:** {:.2} ms\n- **fsync:** {:.2} ms\n",
                human_bytes(write.bytes),
                write.throughput_mib_s(),
                human_bytes(write.chunk_size as u64),
                write.wall_nanos as f64 / 1e6,
                write.fsync_nanos as f64 / 1e6,
            ));
        }
        Report::Random { random } => {
            s.push_str("# Sawskas Profiler — random access\n\n");
            s.push_str(&format!(
                "- **IOPS:** {:.0}\n- **Ops:** {}\n- **Block size:** {}\n- **Avg latency:** {} ns\n- **p99 latency:** {} ns\n- **Mode:** {}\n",
                random.iops(),
                random.ops,
                human_bytes(random.block_size as u64),
                random.avg_nanos,
                random.p99_nanos,
                if random.uncached { "uncached" } else { "cached" },
            ));
        }
    }
    s
}
