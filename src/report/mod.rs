//! Report building & export.
//!
//! Turns measurements into machine-readable (JSON / CSV) and human-facing
//! (HTML with inline SVG graphs) reports. Serialization is hand-rolled — no
//! serde, no SDK — to match the rest of the project.

#![allow(dead_code)]

pub mod csv;
pub mod html;
pub mod json;

use crate::interrupt::hardware_interrupt::{BulkMeasurement, FileMeasurement, SweepResult};
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

    /// Writes `contents` to `path`, returning the path on success.
    pub fn write(path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }
}
