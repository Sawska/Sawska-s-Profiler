//! Hand-rolled CSV serialization.
//!
//! Profile reports emit one row per file; bulk reports emit one row per worker
//! thread. Fields that may contain commas/quotes are double-quoted and escaped.

use super::Report;

/// Quotes a field if it contains a comma, quote, or newline (RFC-4180 style).
fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn render(r: &Report) -> String {
    let mut out = String::new();
    match r {
        Report::Profile { files, .. } => {
            out.push_str(
                "path,size_bytes,bytes_read,wall_nanos,throughput_mib_s,chunk_count,\
avg_chunk_ns,min_chunk_ns,max_chunk_ns,read_busy_fraction,timer_interrupts,counter_hz\n",
            );
            for m in files {
                out.push_str(&format!(
                    "{},{},{},{},{:.4},{},{},{},{},{:.4},{},{}\n",
                    field(&m.path),
                    m.size_bytes,
                    m.bytes_read,
                    m.wall_nanos,
                    m.throughput_mib_s(),
                    m.chunk_count,
                    m.avg_chunk_nanos(),
                    m.min_chunk_nanos(),
                    m.max_chunk_nanos(),
                    m.read_busy_fraction(),
                    m.timer_interrupts,
                    m.counter_hz,
                ));
            }
        }
        Report::Bulk { bulk, .. } => {
            out.push_str("thread_index,files,bytes,wall_nanos,throughput_mib_s\n");
            for t in &bulk.per_thread {
                out.push_str(&format!(
                    "{},{},{},{},{:.4}\n",
                    t.index,
                    t.files,
                    t.bytes,
                    t.wall_nanos,
                    t.throughput_mib_s(),
                ));
            }
        }
        Report::Sweep { sweep } => {
            out.push_str("chunk_size,bytes,chunks,wall_nanos,avg_chunk_ns,throughput_mib_s\n");
            for p in &sweep.points {
                out.push_str(&format!(
                    "{},{},{},{},{},{:.4}\n",
                    p.chunk_size,
                    p.bytes,
                    p.chunks,
                    p.wall_nanos,
                    p.avg_chunk_nanos,
                    p.throughput_mib_s(),
                ));
            }
        }
    }
    out
}
