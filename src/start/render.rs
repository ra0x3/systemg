//! Rendering boot frames to the terminal and deciding the command's verdict.
//!
//! In verbose mode each frame becomes a line (`Starting web...`, `✓ web`,
//! `✗ worker`); in quiet mode nothing is printed here (the caller's spinner is
//! the feedback) but failures are still collected. Either way the [`BootReport`]
//! it produces is the source of truth for the exit code and the diagnostics to
//! show.

use std::io::Write;

use crate::{
    diag::Diagnostic,
    start::{BootFrame, Outcome},
};

/// The result of consuming a boot stream: what came up and what failed.
#[derive(Debug, Default)]
pub struct BootReport {
    /// Units that came up or completed.
    pub started: usize,
    /// Diagnostics for units that failed to come up, in arrival order.
    pub failures: Vec<Diagnostic>,
}

impl BootReport {
    /// Whether every unit that was attempted came up or completed.
    pub fn all_ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Consumes boot frames, rendering progress (when `verbose`) to `out`, and
/// returns the report. `out` is the human-facing stream (stderr).
pub fn render_boot<W: Write>(
    frames: impl IntoIterator<Item = BootFrame>,
    verbose: bool,
    mut out: W,
) -> BootReport {
    let mut report = BootReport::default();
    for frame in frames {
        match frame {
            BootFrame::UnitStarting { service, .. } => {
                if verbose {
                    let _ = writeln!(out, "Starting {service}...");
                }
            }
            BootFrame::Unit {
                service, outcome, ..
            } => match outcome {
                Outcome::Up(live) => {
                    report.started += 1;
                    if verbose {
                        let _ = writeln!(out, "  \u{2713} {service} [pid {}]", live.pid);
                    }
                }
                Outcome::Completed => {
                    report.started += 1;
                    if verbose {
                        let _ = writeln!(out, "  \u{2713} {service} completed");
                    }
                }
                Outcome::Failed(diag) => {
                    if verbose {
                        let _ = writeln!(
                            out,
                            "  \u{2717} {service} \u{2014} {}: {}",
                            diag.code_str(),
                            diag.title
                        );
                    }
                    report.failures.push(diag);
                }
            },
            BootFrame::Done { .. } => {}
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diag::SgCode,
        start::{Liveness, outcome},
    };

    fn starting(service: &str) -> BootFrame {
        BootFrame::UnitStarting {
            project: "p".into(),
            service: service.into(),
        }
    }

    fn up(service: &str, pid: u32) -> BootFrame {
        BootFrame::Unit {
            project: "p".into(),
            service: service.into(),
            outcome: Outcome::Up(Liveness { pid }),
        }
    }

    fn failed(service: &str) -> BootFrame {
        BootFrame::Unit {
            project: "p".into(),
            service: service.into(),
            outcome: Outcome::Failed(outcome::immediate_exit(service, Some(1))),
        }
    }

    #[test]
    fn verbose_prints_a_line_per_service() {
        let frames = vec![starting("web"), up("web", 42), starting("db"), up("db", 43)];
        let mut buf = Vec::new();
        let report = render_boot(frames, true, &mut buf);
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("Starting web..."));
        assert!(text.contains("web [pid 42]"));
        assert!(text.contains("Starting db..."));
        assert_eq!(report.started, 2);
        assert!(report.all_ok());
    }

    #[test]
    fn quiet_prints_nothing_but_still_reports() {
        let frames = vec![starting("web"), up("web", 42), failed("worker")];
        let mut buf = Vec::new();
        let report = render_boot(frames, false, &mut buf);
        assert!(buf.is_empty());
        assert_eq!(report.started, 1);
        assert_eq!(report.failures.len(), 1);
        assert!(!report.all_ok());
    }

    #[test]
    fn failures_collect_their_diagnostics() {
        let frames = vec![
            failed("worker"),
            BootFrame::Done {
                started: 0,
                failed: 1,
            },
        ];
        let mut buf = Vec::new();
        let report = render_boot(frames, true, &mut buf);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].code, SgCode::UnitImmediateExit);
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("SG0102"));
    }
}
