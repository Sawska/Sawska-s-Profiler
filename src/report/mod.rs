//! Report building & export.
//!
//! Turns measurements into machine-readable (JSON / CSV) and human-facing
//! (HTML with inline SVG graphs) reports. Serialization is hand-rolled — no
//! serde, no SDK — to match the rest of the project.

#![allow(dead_code)]

pub mod csv;
pub mod html;
pub mod json;
pub mod markdown;

use crate::interrupt::hardware_interrupt::{
    BulkMeasurement, FileMeasurement, RandomMeasurement, SweepResult, WriteMeasurement,
};
use std::io;
use std::path::Path;

/// How a set of file measurements is ordered for presentation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Slowest read throughput first — surfaces bottlenecks.
    Throughput,
    /// Largest file first.
    Size,
    /// Path, alphabetical.
    Name,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Throughput => "slowest throughput first",
            SortKey::Size => "largest first",
            SortKey::Name => "by name",
        }
    }

    /// Parses a CLI/prompt token; unknown input falls back to `Throughput`.
    pub fn parse(s: &str) -> SortKey {
        match s.trim().to_ascii_lowercase().as_str() {
            "size" | "s" => SortKey::Size,
            "name" | "n" => SortKey::Name,
            _ => SortKey::Throughput,
        }
    }

    /// Sorts measurements in place per this key.
    pub fn sort(self, files: &mut [FileMeasurement]) {
        match self {
            SortKey::Throughput => files.sort_by(|a, b| {
                a.throughput_mib_s()
                    .partial_cmp(&b.throughput_mib_s())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortKey::Size => files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes)),
            SortKey::Name => files.sort_by(|a, b| a.path.cmp(&b.path)),
        }
    }
}

/// Aggregate stats for one file extension.
pub struct ExtStat {
    pub ext: String,
    pub files: u64,
    pub bytes: u64,
    pub wall_nanos: u64,
}

impl ExtStat {
    pub fn throughput_mib_s(&self) -> f64 {
        let secs = self.wall_nanos as f64 / 1e9;
        if secs <= 0.0 {
            0.0
        } else {
            (self.bytes as f64 / (1024.0 * 1024.0)) / secs
        }
    }
}

/// Groups measurements by file extension, sorted by total bytes (descending).
pub fn ext_breakdown(files: &[FileMeasurement]) -> Vec<ExtStat> {
    use std::collections::HashMap;
    let mut map: HashMap<String, ExtStat> = HashMap::new();
    for m in files {
        let ext = std::path::Path::new(&m.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_else(|| "(none)".to_string());
        let e = map.entry(ext.clone()).or_insert(ExtStat {
            ext,
            files: 0,
            bytes: 0,
            wall_nanos: 0,
        });
        e.files += 1;
        e.bytes += m.bytes_read;
        e.wall_nanos += m.wall_nanos;
    }
    let mut v: Vec<ExtStat> = map.into_values().collect();
    v.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    v
}

/// A finished report ready to serialize.
pub enum Report {
    Profile {
        root: String,
        files: Vec<FileMeasurement>,
    },
    Bulk {
        root: String,
        bulk: BulkMeasurement,
    },
    Sweep {
        sweep: SweepResult,
    },
    Write {
        write: WriteMeasurement,
    },
    Random {
        random: RandomMeasurement,
    },
}

impl Report {
    pub fn to_json(&self) -> String {
        json::render(self)
    }

    pub fn to_csv(&self) -> String {
        csv::render(self)
    }

    pub fn to_html(&self) -> String {
        html::render(self)
    }

    pub fn to_markdown(&self) -> String {
        markdown::render(self)
    }

    /// Writes `contents` to `path`, returning the path on success.
    pub fn write(path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }
}
