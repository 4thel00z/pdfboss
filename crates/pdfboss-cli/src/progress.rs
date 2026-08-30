//! Stderr progress reporting for the HTTP full-download fallback: a notice
//! line, then a single bar line redrawn in place and erased when the
//! download ends. Nothing is written unless the fallback actually runs,
//! and never to stdout.

use std::io::IsTerminal as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use pdfboss_aio::{AsyncDocument, CachedBackend, HttpBackend};

const BAR_WIDTH: usize = 30;

/// Bytes rendered with a binary-unit suffix: exact below 1 KiB, one decimal
/// from KiB up.
fn human_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

/// Percent complete, clamping overrun and treating a zero total as done.
fn percent_of(collected: u64, total: u64) -> u64 {
    (collected.min(total) * 100)
        .checked_div(total)
        .unwrap_or(100)
}

/// One bar line for the current download state; overrun and a zero total
/// both clamp to 100%.
fn progress_line(collected: u64, total: u64) -> String {
    let collected = collected.min(total);
    let percent = percent_of(collected, total);
    let filled = (percent as usize * BAR_WIDTH) / 100;
    let bar = if filled == BAR_WIDTH {
        "=".repeat(BAR_WIDTH)
    } else {
        format!(
            "{}>{}",
            "=".repeat(filled),
            " ".repeat(BAR_WIDTH - filled - 1)
        )
    };
    format!(
        "[{bar}] {percent:>3}%  {}/{}",
        human_size(collected),
        human_size(total)
    )
}

/// The stderr renderer behind the fallback-progress callback: a persistent
/// notice line on the first report, a bar line redrawn in place on every
/// percent change, and `finish` erasing the bar line once the open is over
/// (the notice stays).
struct DownloadBar {
    started: AtomicBool,
    last_percent: AtomicU64,
}

impl DownloadBar {
    fn new() -> DownloadBar {
        DownloadBar {
            started: AtomicBool::new(false),
            last_percent: AtomicU64::new(u64::MAX),
        }
    }

    fn tick(&self, collected: u64, total: u64) {
        if !self.started.swap(true, Ordering::SeqCst) {
            eprintln!(
                "pdfboss: server ignores Range requests, downloading the whole file ({})",
                human_size(total)
            );
        }
        let percent = percent_of(collected, total);
        if self.last_percent.swap(percent, Ordering::SeqCst) == percent {
            return;
        }
        eprint!("\r{}", progress_line(collected, total));
        let _ = std::io::stderr().flush();
    }

    fn finish(&self) {
        if !self.started.load(Ordering::SeqCst) {
            return;
        }
        eprint!("\r\x1b[K");
        let _ = std::io::stderr().flush();
    }
}

/// Opens `url` through the HTTP backend stack (`CachedBackend` over
/// `HttpBackend`, like `AsyncDocument::open_url_with_password`), drawing a
/// stderr progress bar during the full-download fallback when stderr is a
/// terminal. A short body or a mid-download error still leaves the terminal
/// on a clean line.
pub async fn open_url_with_progress(
    url: &str,
    password: &str,
) -> pdfboss_aio::Result<AsyncDocument> {
    let backend = HttpBackend::new(url).await?;
    if !std::io::stderr().is_terminal() {
        return AsyncDocument::with_backend_with_password(CachedBackend::new(backend), password)
            .await;
    }
    let bar = Arc::new(DownloadBar::new());
    let ticker = Arc::clone(&bar);
    let backend =
        backend.on_fallback_progress(move |collected, total| ticker.tick(collected, total));
    let doc =
        AsyncDocument::with_backend_with_password(CachedBackend::new(backend), password).await;
    bar.finish();
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_picks_the_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(110_175), "107.6 KiB");
        assert_eq!(human_size(11_165_345), "10.6 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn progress_line_renders_the_bar_states() {
        assert_eq!(
            progress_line(0, 110_175),
            "[>                             ]   0%  0 B/107.6 KiB"
        );
        assert_eq!(
            progress_line(55_000, 110_175),
            "[==============>               ]  49%  53.7 KiB/107.6 KiB"
        );
        assert_eq!(
            progress_line(110_175, 110_175),
            "[==============================] 100%  107.6 KiB/107.6 KiB"
        );
    }

    #[test]
    fn progress_line_clamps_overrun_and_zero_total() {
        assert_eq!(
            progress_line(2000, 1000),
            "[==============================] 100%  1000 B/1000 B"
        );
        assert!(progress_line(0, 0).contains("100%"));
    }
}
