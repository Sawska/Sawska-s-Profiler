# Sawskas Profiler

A low-level file I/O profiler for macOS and Linux, built **without an I/O or
profiling SDK** — every measurement goes through hand-declared `libc` syscalls
and the CPU/system hardware counter read with inline assembly. It ships both a
cyberpunk terminal UI and a scriptable command-line mode, and exports reports as
JSON, CSV, HTML (with inline SVG graphs), and Markdown.

## The honest hardware story

A userspace process **cannot** install an interrupt service routine — the
interrupt vector table (x86 IDT / ARM GIC) lives in ring-0 / EL1, and only the
kernel services a hardware IRQ. What this profiler does instead is consume the
two things the kernel exposes *from* real hardware:

1. **The hardware timer interrupt**, virtualized and delivered as a `SIGALRM`
   via `setitimer`. The CPU timer raises a real IRQ; the kernel handles it and
   posts us a signal. Our async-signal-safe handler runs on that interrupt's
   tail, so the *cadence of sampling is dictated by a hardware interrupt* —
   exactly how `perf`/`gprof` sample.
2. **The hardware system counter**, read with a single instruction and no
   syscall: `CNTVCT_EL0` on `aarch64`, `RDTSCP` on `x86_64`. This gives
   tick-accurate latency for every `read()`.

No `libc` crate, no `std::fs` for the measured paths — files are opened, sized,
read, written, and closed with raw syscalls.

## Modes

| Mode | What it measures |
|---|---|
| **Run / profile** | Per-file read profiling: throughput, per-`read()` latency (avg/min/max, p50/p95/p99), a log-scale latency histogram, and timer-interrupt-driven progress samples. A directory is scanned as a batch with filter, sort, and an aggregate. |
| **Bulk throughput** | Parallel raw reads across N threads with a per-thread breakdown. Answers "how fast can I drain this tree?" — no per-file interrupt sampling (the global timer can't be shared across threads). |
| **Block-size sweep** | Re-reads one file at 4 KiB → 16 MiB block sizes to find the throughput-optimal read size. |
| **Write benchmark** | Sequential write throughput into a target directory, including `fsync` cost. |
| **Random access** | Random-offset reads → IOPS and per-op latency (avg + p99). |

### Cached vs. uncached

By default reads are served from the page cache, so numbers reflect RAM. Pass
`--uncached` (or pick "uncached" at the prompt) to bypass the cache —
`fcntl(F_NOCACHE)` on macOS, `posix_fadvise(DONTNEED)` on Linux — so results
reflect storage. This is best effort: neither call can force-evict pages already
resident from another handle without elevated privileges.

## Usage

Run with no path for the interactive terminal UI:

```sh
cargo run
```

Or headless with a path:

```sh
# Profile a directory of Rust files, write an HTML report with graphs
Sawskas-profiler src --ext rs --html report.html

# Find the best block size for a file, measuring real storage
Sawskas-profiler bigfile.bin --sweep --uncached --json sweep.json

# Parallel bulk read of a tree across 8 threads
Sawskas-profiler . --bulk --threads 8 --md report.md

# Write benchmark into a directory; random-access IOPS on a file
Sawskas-profiler /tmp --write --json write.json
Sawskas-profiler bigfile.bin --random --csv random.csv
```

### Options

```
--bulk                Parallel bulk-throughput mode
--sweep               Block-size sweep on a single file (4 KiB → 16 MiB)
--write               Sequential write benchmark into PATH (a directory)
--random              Random-access IOPS benchmark on PATH (a file)
--uncached            Bypass the page cache (measure storage, not RAM)
--threads N           Worker threads for --bulk (default: CPU count)
--ext LIST            Only files with these extensions, e.g. rs,toml
--all                 Include hidden (dot) files (default: skipped)
--sort KEY            throughput | size | name  (default: throughput)
--json/--csv/--html/--md [FILE]   Write a report in that format
-h, --help            Show help
```

## Reports

- **JSON / CSV** — machine-readable, hand-rolled (no serde).
- **HTML** — self-contained, dark neon theme, inline SVG graphs (throughput,
  latency distribution, per-thread, block-size curves); no charting library.
- **Markdown** — readable summary tables, including a by-file-type breakdown.

In the terminal UI, the `EXPORT REPORT` menu item writes the last run to all
four formats.

## Build & test

```sh
cargo build
cargo test          # the full live-I/O demo is `cargo test live_demo -- --ignored --nocapture`
```

Requires a recent Rust toolchain. Tested on Apple Silicon (macOS); the Linux
paths are `cfg`-gated but less exercised.
