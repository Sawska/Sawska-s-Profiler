//! Hardware-interrupt-driven file measurement.
//!
//! # The honest hardware story
//!
//! A userspace process **cannot** install an interrupt service routine. The
//! interrupt vector table (x86 IDT / ARM GIC) lives in ring-0 / EL1; only the
//! kernel can service a hardware IRQ. What a process like this profiler *can*
//! do is consume the two things the kernel exposes from real hardware:
//!
//! 1. **The hardware timer interrupt**, virtualized and delivered to us as a
//!    `SIGALRM` signal via `setitimer(ITIMER_REAL, …)`. The CPU timer raises a
//!    real IRQ, the kernel handles it, and on its way out posts a signal to
//!    this process. Our handler ([`on_timer`]) runs on the tail of that
//!    interrupt. This is precisely how statistical profilers (`perf`, `gprof`)
//!    sample — the *cadence of measurement is dictated by a hardware interrupt*,
//!    not by us polling a clock.
//!
//! 2. **The hardware system counter**, read straight off the silicon with a
//!    single instruction and no syscall:
//!      * `aarch64`: `mrs x, cntvct_el0` (fixed-frequency system counter)
//!      * `x86_64` : `rdtscp`            (time-stamp counter)
//!    This gives cycle/tick-accurate latency for every `read()` we issue.
//!
//! Everything talks to the OS through hand-declared `libc` symbols — no `libc`
//! crate, no SDK, no `std::fs`. Files are opened, sized, read and closed with
//! raw syscalls; timing comes from inline assembly.

#![allow(dead_code)]

use core::arch::asm;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::ffi::CString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ============================================================
//  Platform constants
// ============================================================

const O_RDONLY: c_int = 0;
const SEEK_SET: c_int = 0;
const SEEK_END: c_int = 2;
const ITIMER_REAL: c_int = 0;
const SIGALRM: c_int = 14; // identical on macOS and Linux
const EINTR: c_int = 4;
const SIG_ERR: usize = usize::MAX; // (sighandler_t)-1

#[cfg(target_os = "macos")]
const CLOCK_MONOTONIC: c_int = 6;
#[cfg(not(target_os = "macos"))]
const CLOCK_MONOTONIC: c_int = 1;

/// `suseconds_t` is `int` on macOS but `long` on Linux — the one field whose
/// width genuinely differs in the structs we touch.
#[cfg(target_os = "macos")]
type Suseconds = i32;
#[cfg(not(target_os = "macos"))]
type Suseconds = i64;

/// 256 KiB read buffer — large enough to amortize syscall overhead, small
/// enough to take several timer-interrupt samples across a sizeable file.
const READ_CHUNK: usize = 256 * 1024;

// ============================================================
//  Raw libc bindings (declared by hand — no `libc` crate)
// ============================================================

#[repr(C)]
struct Timespec {
    tv_sec: i64,  // time_t (LP64)
    tv_nsec: i64, // long   (LP64)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Timeval {
    tv_sec: i64,
    tv_usec: Suseconds,
}

#[repr(C)]
struct Itimerval {
    it_interval: Timeval, // reload value
    it_value: Timeval,    // time until next expiry
}

unsafe extern "C" {
    // `open` is variadic in C (`int open(const char*, int, ...)`); we never pass
    // a mode (read-only), so a two-arg declaration is ABI-correct here.
    fn open(path: *const c_char, oflag: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    fn clock_gettime(clk_id: c_int, tp: *mut Timespec) -> c_int;
    fn setitimer(which: c_int, new: *const Itimerval, old: *mut Itimerval) -> c_int;
    // BSD-semantics `signal()` (persistent handler + SA_RESTART) on both macOS
    // and glibc — avoids the wildly platform-divergent `struct sigaction`.
    fn signal(signum: c_int, handler: extern "C" fn(c_int)) -> usize;
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn __error() -> *mut c_int;
}
#[cfg(not(target_os = "macos"))]
unsafe extern "C" {
    fn __errno_location() -> *mut c_int;
}

#[inline]
fn errno() -> c_int {
    #[cfg(target_os = "macos")]
    unsafe {
        *__error()
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        *__errno_location()
    }
}

// ============================================================
//  Hardware counter — read off the silicon, no syscall
// ============================================================

/// Reads the CPU/system hardware counter with a single instruction.
#[inline(always)]
fn read_counter() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // CNTVCT_EL0: the architected virtual system counter. Readable from
        // EL0 on macOS/Linux; ticks at a fixed frequency (CNTFRQ_EL0).
        unsafe { asm!("mrs {v}, cntvct_el0", v = out(reg) v, options(nomem, nostack)) };
        v
    }
    #[cfg(target_arch = "x86_64")]
    {
        let lo: u32;
        let hi: u32;
        let _aux: u32;
        // RDTSCP serializes against prior loads/stores better than RDTSC and
        // also yields the core id in ECX (discarded here).
        unsafe {
            asm!("rdtscp", out("eax") lo, out("edx") hi, out("ecx") _aux, options(nomem, nostack))
        };
        ((hi as u64) << 32) | (lo as u64)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        monotonic_nanos() // graceful fallback: nanoseconds as "ticks"
    }
}

/// Frequency of [`read_counter`] in Hz, so ticks convert to real time.
fn counter_hz() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let hz: u64;
        unsafe { asm!("mrs {hz}, cntfrq_el0", hz = out(reg) hz, options(nomem, nostack)) };
        hz
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // No architectural frequency register (x86 TSC) — calibrate the counter
        // against CLOCK_MONOTONIC over a short busy interval.
        let t0 = monotonic_nanos();
        let c0 = read_counter();
        while monotonic_nanos().wrapping_sub(t0) < 20_000_000 {} // ~20 ms
        let dt = monotonic_nanos().wrapping_sub(t0);
        let dc = read_counter().wrapping_sub(c0);
        if dt == 0 {
            0
        } else {
            ((dc as u128 * 1_000_000_000u128) / dt as u128) as u64
        }
    }
}

#[inline]
fn monotonic_nanos() -> u64 {
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
    (ts.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec as u64)
}

#[inline]
fn ticks_to_nanos(ticks: u64, hz: u64) -> u64 {
    if hz == 0 {
        0
    } else {
        ((ticks as u128 * 1_000_000_000u128) / hz as u128) as u64
    }
}

// ============================================================
//  Hardware-timer interrupt handler
// ============================================================
//
// The handler must be async-signal-safe: it touches nothing but lock-free
// atomics. It records that the hardware timer fired and flags that the main
// loop should snapshot progress at the next chunk boundary. The *timing* of
// these samples is therefore driven entirely by the hardware interrupt.

static TIMER_INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static SAMPLE_PENDING: AtomicBool = AtomicBool::new(false);
/// Process-wide guard: the timer/handler are global, so only one measurement
/// may be armed at a time.
static MEASURING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_timer(_sig: c_int) {
    TIMER_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
    SAMPLE_PENDING.store(true, Ordering::Relaxed);
}

/// RAII guard so the global `MEASURING` lock is always released, even on an
/// early `?`-return.
struct MeasureGuard;
impl Drop for MeasureGuard {
    fn drop(&mut self) {
        MEASURING.store(false, Ordering::Release);
    }
}

// ============================================================
//  Public result types
// ============================================================

/// Live progress handed to a [`measure_file_with`] callback whenever the
/// hardware timer interrupt produces a snapshot.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub bytes_read: u64,
    pub size_bytes: u64,
    /// Monotonic nanoseconds since measurement start.
    pub elapsed_nanos: u64,
    /// Which timer interrupt produced this update (1-based).
    pub interrupt_index: u64,
}

/// Upper bounds (nanoseconds) for the read-latency histogram buckets. A final
/// overflow bucket catches anything slower than the last bound.
pub const LATENCY_BOUNDS_NS: [u64; 13] = [
    1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000, 500_000,
    1_000_000, 2_000_000, 5_000_000, 10_000_000,
];
/// Total histogram buckets (one per bound + a trailing overflow bucket).
pub const LATENCY_BUCKETS: usize = LATENCY_BOUNDS_NS.len() + 1;

/// Maps a chunk latency (ns) to its histogram bucket index.
#[inline]
fn latency_bucket(nanos: u64) -> usize {
    for (i, bound) in LATENCY_BOUNDS_NS.iter().enumerate() {
        if nanos <= *bound {
            return i;
        }
    }
    LATENCY_BOUNDS_NS.len()
}

/// One progress snapshot, taken because the hardware timer interrupt fired.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Monotonic nanoseconds since measurement start.
    pub at_nanos: u64,
    /// Bytes read by the time this interrupt was serviced.
    pub bytes: u64,
    /// Which timer interrupt produced this sample (1-based).
    pub interrupt_index: u64,
}

/// The full, real measurement of a file.
#[derive(Debug, Clone)]
pub struct FileMeasurement {
    pub path: String,
    pub size_bytes: u64,
    pub bytes_read: u64,
    pub wall_nanos: u64,
    /// Hardware-counter ticks spent inside `read()` syscalls.
    pub read_ticks: u64,
    pub chunk_count: u64,
    pub min_chunk_ticks: u64,
    pub max_chunk_ticks: u64,
    /// Number of hardware timer interrupts that fired during the read.
    pub timer_interrupts: u64,
    pub counter_hz: u64,
    pub samples: Vec<Sample>,
    /// Per-chunk read-latency distribution; see [`LATENCY_BOUNDS_NS`].
    pub latency_buckets: [u64; LATENCY_BUCKETS],
}

impl FileMeasurement {
    pub fn throughput_mib_s(&self) -> f64 {
        let secs = self.wall_nanos as f64 / 1e9;
        if secs <= 0.0 {
            0.0
        } else {
            (self.bytes_read as f64 / (1024.0 * 1024.0)) / secs
        }
    }

    pub fn avg_chunk_nanos(&self) -> u64 {
        if self.chunk_count == 0 {
            0
        } else {
            ticks_to_nanos(self.read_ticks / self.chunk_count, self.counter_hz)
        }
    }

    pub fn min_chunk_nanos(&self) -> u64 {
        ticks_to_nanos(self.min_chunk_ticks, self.counter_hz)
    }

    pub fn max_chunk_nanos(&self) -> u64 {
        ticks_to_nanos(self.max_chunk_ticks, self.counter_hz)
    }

    /// Fraction of wall time actually spent blocked in `read()` (0.0–1.0).
    pub fn read_busy_fraction(&self) -> f64 {
        if self.wall_nanos == 0 {
            0.0
        } else {
            ticks_to_nanos(self.read_ticks, self.counter_hz) as f64 / self.wall_nanos as f64
        }
    }
}

impl fmt::Display for FileMeasurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "── FILE MEASUREMENT ─────────────────────────")?;
        writeln!(f, "  path            {}", self.path)?;
        writeln!(f, "  size            {} bytes", self.size_bytes)?;
        writeln!(f, "  read            {} bytes", self.bytes_read)?;
        writeln!(
            f,
            "  wall time       {:.3} ms",
            self.wall_nanos as f64 / 1e6
        )?;
        writeln!(f, "  throughput      {:.2} MiB/s", self.throughput_mib_s())?;
        writeln!(f, "  chunks          {}", self.chunk_count)?;
        writeln!(
            f,
            "  read latency    avg {} ns · min {} ns · max {} ns",
            self.avg_chunk_nanos(),
            self.min_chunk_nanos(),
            self.max_chunk_nanos()
        )?;
        writeln!(
            f,
            "  read-busy       {:.1}% of wall time",
            self.read_busy_fraction() * 100.0
        )?;
        writeln!(
            f,
            "  timer IRQs      {} (hw counter @ {} Hz)",
            self.timer_interrupts, self.counter_hz
        )?;
        write!(f, "  samples         {} captured", self.samples.len())
    }
}

// ============================================================
//  The measurement itself
// ============================================================

/// Measures a file end-to-end using raw syscalls, hardware-counter timing, and
/// hardware-timer-interrupt-driven sampling.
///
/// `sample_interval` sets how often the hardware timer fires (e.g. 2 ms). Each
/// firing flags a progress snapshot recorded at the next chunk boundary.
pub fn measure_file(path: &str, sample_interval: Duration) -> io::Result<FileMeasurement> {
    measure_file_with(path, sample_interval, |_| {})
}

/// Like [`measure_file`], but invokes `on_sample` each time the hardware timer
/// interrupt produces a snapshot — ideal for driving a live UI whose refresh
/// cadence is itself dictated by the hardware interrupt. The callback runs on
/// the main thread at a chunk boundary (never inside the signal handler), so it
/// may freely do I/O.
pub fn measure_file_with<F: FnMut(Progress)>(
    path: &str,
    sample_interval: Duration,
    mut on_sample: F,
) -> io::Result<FileMeasurement> {
    // Acquire the process-wide guard — the timer + handler are global state.
    if MEASURING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another measurement is already in progress",
        ));
    }
    let _guard = MeasureGuard;

    let c_path = CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;

    // ── open() ───────────────────────────────────────────────────────────
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    let _fd_guard = FdGuard(fd);

    // ── size via lseek(END) then rewind ──────────────────────────────────
    let size = unsafe { lseek(fd, 0, SEEK_END) };
    if size < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    if unsafe { lseek(fd, 0, SEEK_SET) } < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }

    let hz = counter_hz();

    // ── arm the hardware timer interrupt ─────────────────────────────────
    TIMER_INTERRUPTS.store(0, Ordering::Relaxed);
    SAMPLE_PENDING.store(false, Ordering::Relaxed);
    if unsafe { signal(SIGALRM, on_timer) } == SIG_ERR {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    arm_timer(sample_interval)?;

    // ── the read loop ────────────────────────────────────────────────────
    let mut buf = vec![0u8; READ_CHUNK];
    let mut bytes_read: u64 = 0;
    let mut read_ticks: u64 = 0;
    let mut chunk_count: u64 = 0;
    let mut min_chunk_ticks = u64::MAX;
    let mut max_chunk_ticks = 0u64;
    let mut latency_buckets = [0u64; LATENCY_BUCKETS];
    let mut samples: Vec<Sample> = Vec::new();

    let start_ns = monotonic_nanos();

    loop {
        let t0 = read_counter();
        let n = unsafe { read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        let dt = read_counter().wrapping_sub(t0);

        if n < 0 {
            // With BSD signal() reads auto-restart, but stay defensive.
            if errno() == EINTR {
                continue;
            }
            disarm_timer();
            return Err(io::Error::from_raw_os_error(errno()));
        }
        if n == 0 {
            break; // EOF
        }

        bytes_read += n as u64;
        read_ticks += dt;
        chunk_count += 1;
        min_chunk_ticks = min_chunk_ticks.min(dt);
        max_chunk_ticks = max_chunk_ticks.max(dt);
        latency_buckets[latency_bucket(ticks_to_nanos(dt, hz))] += 1;

        // Did a hardware timer interrupt fire since the last chunk? If so,
        // snapshot progress now. The cadence is the interrupt's, not ours.
        if SAMPLE_PENDING.swap(false, Ordering::Relaxed) {
            let at = monotonic_nanos().wrapping_sub(start_ns);
            let irq = TIMER_INTERRUPTS.load(Ordering::Relaxed);
            samples.push(Sample {
                at_nanos: at,
                bytes: bytes_read,
                interrupt_index: irq,
            });
            on_sample(Progress {
                bytes_read,
                size_bytes: size as u64,
                elapsed_nanos: at,
                interrupt_index: irq,
            });
        }
    }

    let wall_nanos = monotonic_nanos().wrapping_sub(start_ns);
    disarm_timer();

    if chunk_count == 0 {
        min_chunk_ticks = 0;
    }

    Ok(FileMeasurement {
        path: path.to_string(),
        size_bytes: size as u64,
        bytes_read,
        wall_nanos,
        read_ticks,
        chunk_count,
        min_chunk_ticks,
        max_chunk_ticks,
        timer_interrupts: TIMER_INTERRUPTS.load(Ordering::Relaxed),
        counter_hz: hz,
        samples,
        latency_buckets,
    })
}

/// Closes `fd` no matter how we leave [`measure_file`].
struct FdGuard(c_int);
impl Drop for FdGuard {
    fn drop(&mut self) {
        unsafe { close(self.0) };
    }
}

fn timeval_from_duration(d: Duration) -> Timeval {
    Timeval {
        tv_sec: d.as_secs() as i64,
        tv_usec: (d.subsec_micros() as i64) as Suseconds,
    }
}

fn arm_timer(interval: Duration) -> io::Result<()> {
    // A zero interval would disarm; clamp to at least 1 µs.
    let iv = if interval.is_zero() {
        Duration::from_micros(1)
    } else {
        interval
    };
    let tv = timeval_from_duration(iv);
    let it = Itimerval {
        it_interval: tv,
        it_value: tv,
    };
    if unsafe { setitimer(ITIMER_REAL, &it, core::ptr::null_mut()) } != 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    Ok(())
}

fn disarm_timer() {
    let zero = Itimerval {
        it_interval: Timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        it_value: Timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
    };
    unsafe { setitimer(ITIMER_REAL, &zero, core::ptr::null_mut()) };
}

// ============================================================
//  Parallel bulk-throughput mode
// ============================================================
//
// A deliberately *different* measurement model from the single-file profiler:
// raw threaded reads with NO hardware-timer sampling. The process-global timer
// (setitimer/SIGALRM) can only be armed once and its signal lands on an
// arbitrary thread, so per-file interrupt sampling is incompatible with
// parallelism. This mode therefore answers a different question — "how fast can
// I drain this tree?" — timing the whole concurrent operation with the hardware
// counter and reporting aggregate throughput. Per-file latency is intentionally
// not attributed here (concurrent reads contend and would skew it).

/// Per-worker-thread breakdown of a bulk read.
#[derive(Debug, Clone)]
pub struct ThreadStat {
    pub index: usize,
    pub files: u64,
    pub bytes: u64,
    pub wall_nanos: u64,
}

impl ThreadStat {
    pub fn throughput_mib_s(&self) -> f64 {
        let secs = self.wall_nanos as f64 / 1e9;
        if secs <= 0.0 {
            0.0
        } else {
            (self.bytes as f64 / (1024.0 * 1024.0)) / secs
        }
    }
}

#[derive(Debug, Clone)]
pub struct BulkMeasurement {
    pub file_count: u64,
    pub bytes_read: u64,
    pub wall_nanos: u64,
    pub threads: usize,
    pub errors: u64,
    pub counter_hz: u64,
    pub counter_ticks: u64,
    pub per_thread: Vec<ThreadStat>,
}

impl BulkMeasurement {
    pub fn throughput_mib_s(&self) -> f64 {
        let secs = self.wall_nanos as f64 / 1e9;
        if secs <= 0.0 {
            0.0
        } else {
            (self.bytes_read as f64 / (1024.0 * 1024.0)) / secs
        }
    }
}

/// Reads a single file to EOF with raw syscalls, returning the byte count.
fn read_file_raw(path: &str, buf: &mut [u8]) -> io::Result<u64> {
    let c_path = CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    let _fd_guard = FdGuard(fd);

    let mut total = 0u64;
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n < 0 {
            if errno() == EINTR {
                continue;
            }
            return Err(io::Error::from_raw_os_error(errno()));
        }
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    Ok(total)
}

/// Reads `files` concurrently across `threads` worker threads, timing the whole
/// operation with the hardware counter and reporting aggregate throughput.
pub fn measure_bulk(files: &[PathBuf], threads: usize) -> BulkMeasurement {
    let threads = threads.clamp(1, 256);
    let n = files.len();
    // Ceil-divide so every file is covered by exactly one contiguous slice.
    let per = n.div_ceil(threads).max(1);

    let hz = counter_hz();
    let start_ticks = read_counter();
    let start_ns = monotonic_nanos();

    let mut total_bytes = 0u64;
    let mut total_errors = 0u64;
    let mut per_thread: Vec<ThreadStat> = Vec::new();

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (ti, chunk) in files.chunks(per).enumerate() {
            handles.push(scope.spawn(move || {
                let t0 = monotonic_nanos();
                let mut bytes = 0u64;
                let mut errors = 0u64;
                let mut buf = vec![0u8; READ_CHUNK];
                for f in chunk {
                    match f.to_str() {
                        Some(p) => match read_file_raw(p, &mut buf) {
                            Ok(b) => bytes += b,
                            Err(_) => errors += 1,
                        },
                        None => errors += 1,
                    }
                }
                let wall_nanos = monotonic_nanos().wrapping_sub(t0);
                (
                    ThreadStat {
                        index: ti,
                        files: chunk.len() as u64,
                        bytes,
                        wall_nanos,
                    },
                    errors,
                )
            }));
        }
        for h in handles {
            if let Ok((stat, e)) = h.join() {
                total_bytes += stat.bytes;
                total_errors += e;
                per_thread.push(stat);
            }
        }
    });

    let wall_nanos = monotonic_nanos().wrapping_sub(start_ns);
    let counter_ticks = read_counter().wrapping_sub(start_ticks);

    BulkMeasurement {
        file_count: n as u64,
        bytes_read: total_bytes,
        wall_nanos,
        threads,
        errors: total_errors,
        counter_hz: hz,
        counter_ticks,
        per_thread,
    }
}

// ============================================================
//  Chunk-size sweep (block-size benchmark)
// ============================================================
//
// Re-reads one file at a range of read() buffer sizes to find the block size
// that maximizes throughput. A pure throughput benchmark — no timer/signal,
// so it doesn't touch the process-global timer. A warm-up pass first stabilizes
// the page cache so each point isolates the chunk-size variable rather than
// cold-vs-warm cache state.

/// Default block sizes swept: 4 KiB → 16 MiB.
pub const DEFAULT_SWEEP_SIZES: [usize; 7] = [
    4 * 1024,
    16 * 1024,
    64 * 1024,
    256 * 1024,
    1024 * 1024,
    4 * 1024 * 1024,
    16 * 1024 * 1024,
];

#[derive(Debug, Clone)]
pub struct SweepPoint {
    pub chunk_size: usize,
    pub bytes: u64,
    pub chunks: u64,
    pub wall_nanos: u64,
    pub avg_chunk_nanos: u64,
}

impl SweepPoint {
    pub fn throughput_mib_s(&self) -> f64 {
        let secs = self.wall_nanos as f64 / 1e9;
        if secs <= 0.0 {
            0.0
        } else {
            (self.bytes as f64 / (1024.0 * 1024.0)) / secs
        }
    }
}

#[derive(Debug, Clone)]
pub struct SweepResult {
    pub path: String,
    pub size_bytes: u64,
    pub counter_hz: u64,
    pub points: Vec<SweepPoint>,
}

impl SweepResult {
    /// The block size with the highest measured throughput, if any.
    pub fn best(&self) -> Option<&SweepPoint> {
        self.points.iter().max_by(|a, b| {
            a.throughput_mib_s()
                .partial_cmp(&b.throughput_mib_s())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }
}

/// Reads `path` to EOF on `fd` using a `chunk` byte buffer, timing each read
/// with the hardware counter. Returns (bytes, chunks, total_ticks).
fn read_pass(fd: c_int, chunk: usize) -> io::Result<(u64, u64, u64)> {
    if unsafe { lseek(fd, 0, SEEK_SET) } < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    let mut buf = vec![0u8; chunk.max(1)];
    let (mut bytes, mut chunks, mut ticks) = (0u64, 0u64, 0u64);
    loop {
        let t0 = read_counter();
        let n = unsafe { read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        let dt = read_counter().wrapping_sub(t0);
        if n < 0 {
            if errno() == EINTR {
                continue;
            }
            return Err(io::Error::from_raw_os_error(errno()));
        }
        if n == 0 {
            break;
        }
        bytes += n as u64;
        chunks += 1;
        ticks += dt;
    }
    Ok((bytes, chunks, ticks))
}

/// Sweeps `path` across the given block `sizes`, reporting throughput per size.
pub fn sweep_file(path: &str, sizes: &[usize]) -> io::Result<SweepResult> {
    let c_path = CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    let _fd_guard = FdGuard(fd);

    let size = unsafe { lseek(fd, 0, SEEK_END) };
    if size < 0 {
        return Err(io::Error::from_raw_os_error(errno()));
    }
    let hz = counter_hz();

    // Warm-up pass so subsequent points measure a consistent cache state.
    let _ = read_pass(fd, 256 * 1024)?;

    let mut points = Vec::with_capacity(sizes.len());
    for &chunk in sizes {
        let start = read_counter();
        let (bytes, chunks, ticks) = read_pass(fd, chunk)?;
        let wall_nanos = ticks_to_nanos(read_counter().wrapping_sub(start), hz);
        let avg_chunk_nanos = if chunks > 0 {
            ticks_to_nanos(ticks / chunks, hz)
        } else {
            0
        };
        points.push(SweepPoint {
            chunk_size: chunk.max(1),
            bytes,
            chunks,
            wall_nanos,
            avg_chunk_nanos,
        });
    }

    Ok(SweepResult {
        path: path.to_string(),
        size_bytes: size as u64,
        counter_hz: hz,
        points,
    })
}

// ============================================================
//  File discovery
// ============================================================

/// Controls which entries [`collect_files_filtered`] keeps during discovery.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Skip entries whose name begins with '.', including dot-directories.
    pub skip_hidden: bool,
    /// If non-empty, keep only files whose (case-insensitive) extension — with
    /// no leading dot — appears in this list.
    pub extensions: Vec<String>,
}

impl Filter {
    fn accepts_file(&self, p: &Path) -> bool {
        if self.skip_hidden && is_hidden(p) {
            return false;
        }
        if self.extensions.is_empty() {
            return true;
        }
        match p.extension().and_then(|e| e.to_str()) {
            Some(ext) => self.extensions.iter().any(|w| w.eq_ignore_ascii_case(ext)),
            None => false,
        }
    }

    fn enters_dir(&self, p: &Path) -> bool {
        !(self.skip_hidden && is_hidden(p))
    }
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

/// Resolves `root` to the list of regular files to measure, unfiltered.
///
/// * a file  → `[root]`
/// * a directory → every regular file beneath it, recursively
pub fn collect_files(root: &str) -> io::Result<Vec<PathBuf>> {
    collect_files_filtered(root, &Filter::default())
}

/// Like [`collect_files`], but applies `filter` while walking a directory.
///
/// An explicitly named file is always honored regardless of the filter (the
/// user asked for it by name). Traversal uses `std::fs` (the *discovery* layer
/// — the measurement itself stays on raw syscalls). Symlinks are not followed,
/// so the walk can't loop; unreadable sub-directories are skipped rather than
/// aborting the whole walk. Results are sorted for deterministic ordering.
pub fn collect_files_filtered(root: &str, filter: &Filter) -> io::Result<Vec<PathBuf>> {
    let root_path = Path::new(root);
    let meta = std::fs::symlink_metadata(root_path)?;

    if meta.is_file() {
        return Ok(vec![root_path.to_path_buf()]);
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is neither a regular file nor a directory",
        ));
    }

    let mut files = Vec::new();
    let mut stack = vec![root_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // Skip directories we cannot read instead of failing the whole walk.
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {
                    if filter.enters_dir(&p) {
                        stack.push(p);
                    }
                }
                Ok(m) if m.is_file() => {
                    if filter.accepts_file(&p) {
                        files.push(p);
                    }
                }
                _ => {} // symlink / special file / vanished — skip
            }
        }
    }
    files.sort();
    Ok(files)
}

// ============================================================
//  Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that arm the process-global hardware timer, since the
    /// test harness runs them concurrently and the timer is single-armed.
    static TIMER_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Locks [`TIMER_TEST_LOCK`], tolerating a poisoned lock from a prior panic.
    fn timer_guard() -> std::sync::MutexGuard<'static, ()> {
        TIMER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn counter_is_monotonic_and_has_a_frequency() {
        let a = read_counter();
        let b = read_counter();
        assert!(b >= a, "hardware counter went backwards");
        assert!(counter_hz() > 0, "could not determine counter frequency");
    }

    #[test]
    fn measures_a_real_file() {
        let _g = timer_guard();
        // Cargo runs tests from the crate root, so this path exists.
        let m = measure_file("Cargo.toml", Duration::from_micros(500))
            .expect("measurement failed");
        assert!(m.size_bytes > 0);
        assert_eq!(m.bytes_read, m.size_bytes);
        assert!(m.chunk_count >= 1);
        assert!(m.counter_hz > 0);
    }

    #[test]
    fn missing_file_is_an_error() {
        let r = measure_file("definitely/not/here.xyz", Duration::from_millis(1));
        assert!(r.is_err());
    }

    #[test]
    fn collect_single_file_returns_one() {
        let files = collect_files("Cargo.toml").unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("Cargo.toml"));
    }

    #[test]
    fn collect_directory_walks_recursively() {
        let files = collect_files("src").unwrap();
        assert!(files.iter().any(|p| p.ends_with("frontend/ui.rs")));
        assert!(files
            .iter()
            .any(|p| p.ends_with("interrupt/hardware_interrupt.rs")));
        assert!(files.len() >= 3, "expected several source files");
        // Sorted + no directories slipped through.
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
    }

    #[test]
    fn collect_missing_path_is_an_error() {
        assert!(collect_files("definitely/not/here").is_err());
    }

    #[test]
    fn filter_by_extension_keeps_only_matches() {
        let f = Filter {
            skip_hidden: true,
            extensions: vec!["rs".into()],
        };
        let files = collect_files_filtered("src", &f).unwrap();
        assert!(!files.is_empty());
        assert!(files
            .iter()
            .all(|p| p.extension().and_then(|e| e.to_str()) == Some("rs")));
        assert!(files.iter().any(|p| p.ends_with("main.rs")));
    }

    #[test]
    fn latency_histogram_accounts_for_every_chunk() {
        let _g = timer_guard();
        let m = measure_file("Cargo.lock", Duration::from_millis(1))
            .or_else(|_| measure_file("Cargo.toml", Duration::from_millis(1)))
            .unwrap();
        let bucketed: u64 = m.latency_buckets.iter().sum();
        assert_eq!(
            bucketed, m.chunk_count,
            "every measured chunk must land in exactly one bucket"
        );
    }

    #[test]
    fn sweep_reads_full_file_at_every_size() {
        let sizes = [4 * 1024, 64 * 1024, 1024 * 1024];
        let s = sweep_file("Cargo.lock", &sizes)
            .or_else(|_| sweep_file("Cargo.toml", &sizes))
            .unwrap();
        assert_eq!(s.points.len(), sizes.len());
        for p in &s.points {
            assert_eq!(p.bytes, s.size_bytes, "each pass must read the whole file");
        }
        assert!(s.best().is_some());
        assert!(s.counter_hz > 0);
    }

    #[test]
    fn bulk_reads_every_byte() {
        let files = collect_files(
            "src",
        )
        .unwrap();
        let expected: u64 = files
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .sum();

        let m = measure_bulk(&files, 4);
        assert_eq!(m.file_count, files.len() as u64);
        assert_eq!(m.errors, 0);
        assert_eq!(m.bytes_read, expected, "bulk byte total must match metadata");
        assert!(m.counter_hz > 0);
    }

    #[test]
    fn filter_skips_hidden_entries() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("sawskas_filter_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join(".hidden"))
            .unwrap()
            .write_all(b"x")
            .unwrap();
        std::fs::File::create(dir.join("visible.rs"))
            .unwrap()
            .write_all(b"x")
            .unwrap();

        let f = Filter {
            skip_hidden: true,
            extensions: vec![],
        };
        let files = collect_files_filtered(dir.to_str().unwrap(), &f).unwrap();
        assert!(files.iter().any(|p| p.ends_with("visible.rs")));
        assert!(!files.iter().any(|p| p.ends_with(".hidden")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live demo against a real, sizeable file so the hardware-timer interrupt
    /// actually fires and produces samples. Run with:
    ///   cargo test live_demo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_demo() {
        let _g = timer_guard();
        use std::io::Write;

        // Lay down ~96 MiB so the read takes long enough for several timer
        // interrupts to land.
        let path = std::env::temp_dir().join("sawskas_profiler_demo.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            let block = vec![0xABu8; 1 << 20]; // 1 MiB
            for _ in 0..96 {
                f.write_all(&block).unwrap();
            }
            f.flush().unwrap();
        }

        let m = measure_file(path.to_str().unwrap(), Duration::from_millis(2)).unwrap();
        println!("\n{m}\n");
        for s in &m.samples {
            println!(
                "  IRQ #{:>3}  t={:>7.2} ms  read={:>6.2} MiB",
                s.interrupt_index,
                s.at_nanos as f64 / 1e6,
                s.bytes as f64 / (1024.0 * 1024.0),
            );
        }

        let _ = std::fs::remove_file(&path);
        assert_eq!(m.bytes_read, m.size_bytes);
    }
}
