//! Stderr progress reporting for HTTP opens, never touching stdout, drawn
//! only when stderr is a terminal, and erased once the document is open.
//! Two renderers: a notice plus bar for the full-download fallback, and a
//! two-line coverage minimap for ranged opens: a caret marking the byte
//! region being fetched over a map of which stretches of the file have
//! arrived, fed by the read cache's fetch observer.

use std::io::IsTerminal as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pdfboss_aio::{AsyncDocument, Backend as _, CachedBackend, HttpBackend};

const BAR_WIDTH: usize = 30;
const MAP_WIDTH: usize = 40;

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

/// The map cell a byte offset falls into, clamping past-the-end offsets to
/// the last cell and a zero total to the first.
fn cell_of(offset: u64, total: u64, width: usize) -> usize {
    if total == 0 {
        return 0;
    }
    (offset.min(total - 1) * width as u64 / total) as usize
}

/// Marks the cells covered by a fetch of `len` bytes at `offset`.
fn mark_fetch(cells: &mut [bool], offset: u64, len: u64, total: u64) {
    if len == 0 || total == 0 {
        return;
    }
    let first = cell_of(offset, total, cells.len());
    let last = cell_of(offset + len - 1, total, cells.len());
    for cell in &mut cells[first..=last] {
        *cell = true;
    }
}

/// The caret row: `▼` above the cell being fetched right now.
fn caret_line(cell: usize) -> String {
    format!("{}▼", " ".repeat(2 + cell))
}

/// The coverage row: fetched stretches solid, the rest shaded, then the
/// running byte count over the file size. Fetched can exceed the total:
/// evicted chunks fetched again are real traffic, and the count stays
/// honest about it.
fn map_line(cells: &[bool], fetched: u64, total: u64) -> String {
    let map: String = cells.iter().map(|&on| if on { '█' } else { '░' }).collect();
    format!("  {map}   {} / {}", human_size(fetched), human_size(total))
}

/// The stderr renderer behind the read cache's fetch observer: a two-line
/// coverage minimap (caret row over map row) redrawn in place per fetch and
/// erased by `finish` once the open is over. `finish` also disarms it for
/// good: the observer stays attached to the cache for the document's life,
/// and fetches after the open (query evaluation, the TUI's alternate
/// screen) must never draw.
struct OpenMinimap {
    total: u64,
    state: Mutex<MinimapState>,
}

struct MinimapState {
    cells: Vec<bool>,
    fetched: u64,
    started: bool,
    finished: bool,
}

impl OpenMinimap {
    fn new(total: u64) -> OpenMinimap {
        OpenMinimap {
            total,
            state: Mutex::new(MinimapState {
                cells: vec![false; MAP_WIDTH],
                fetched: 0,
                started: false,
                finished: false,
            }),
        }
    }

    /// The stderr payload for one fetch: the first frame draws both lines,
    /// later frames reclaim and redraw them. None once `finish` ran.
    fn frame(&self, offset: u64, len: u64) -> Option<String> {
        let mut state = self.state.lock().expect("minimap mutex");
        if state.finished {
            return None;
        }
        mark_fetch(&mut state.cells, offset, len, self.total);
        state.fetched += len;
        let caret = caret_line(cell_of(offset, self.total, MAP_WIDTH));
        let map = map_line(&state.cells, state.fetched, self.total);
        if state.started {
            return Some(format!("\r\x1b[1A\x1b[K{caret}\n\x1b[K{map}"));
        }
        state.started = true;
        Some(format!("{caret}\n{map}"))
    }

    fn tick(&self, offset: u64, len: u64) {
        let Some(frame) = self.frame(offset, len) else {
            return;
        };
        eprint!("{frame}");
        let _ = std::io::stderr().flush();
    }

    fn finish(&self) {
        let mut state = self.state.lock().expect("minimap mutex");
        let erase = state.started && !state.finished;
        state.finished = true;
        if !erase {
            return;
        }
        eprint!("\r\x1b[K\x1b[1A\x1b[K");
        let _ = std::io::stderr().flush();
    }
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

    fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }
}

/// Opens `url` through the HTTP backend stack (`CachedBackend` over
/// `HttpBackend`, like `AsyncDocument::open_url_with_password`), drawing on
/// stderr when it is a terminal: the coverage minimap during a ranged open,
/// or the notice and progress bar when the full-download fallback runs (the
/// fallback suppresses the minimap, since once the body is resident fetches
/// are free and the map would only flicker). Both erase themselves, so a
/// short body or a mid-open error still leaves the terminal on a clean line.
pub async fn open_url_with_progress(
    url: &str,
    password: &str,
) -> pdfboss_aio::Result<AsyncDocument> {
    let backend = HttpBackend::new(url).await?;
    if !std::io::stderr().is_terminal() {
        return AsyncDocument::with_backend_with_password(CachedBackend::new(backend), password)
            .await;
    }
    let total = backend.len().await.map_err(pdfboss_aio::Error::from)?;
    let bar = Arc::new(DownloadBar::new());
    let ticker = Arc::clone(&bar);
    let backend =
        backend.on_fallback_progress(move |collected, total| ticker.tick(collected, total));
    let map = Arc::new(OpenMinimap::new(total));
    let painter = Arc::clone(&map);
    let fallback = Arc::clone(&bar);
    let cached = CachedBackend::new(backend).on_fetch(move |offset, len| {
        if fallback.is_started() {
            return;
        }
        painter.tick(offset, len);
    });
    let doc = AsyncDocument::with_backend_with_password(cached, password).await;
    map.finish();
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

    #[test]
    fn cell_of_scales_offsets_into_the_map() {
        assert_eq!(cell_of(0, 1000, 10), 0);
        assert_eq!(cell_of(500, 1000, 10), 5);
        assert_eq!(cell_of(999, 1000, 10), 9);
        assert_eq!(cell_of(2000, 1000, 10), 9);
        assert_eq!(cell_of(0, 0, 10), 0);
    }

    #[test]
    fn mark_fetch_covers_the_scaled_byte_range() {
        let mut cells = vec![false; 10];
        mark_fetch(&mut cells, 100, 200, 1000);
        assert_eq!(map_line(&cells, 200, 1000), "  ░██░░░░░░░   200 B / 1000 B");
        mark_fetch(&mut cells, 950, 50, 1000);
        assert_eq!(map_line(&cells, 250, 1000), "  ░██░░░░░░█   250 B / 1000 B");
        mark_fetch(&mut cells, 0, 0, 1000);
        assert_eq!(map_line(&cells, 250, 1000), "  ░██░░░░░░█   250 B / 1000 B");
    }

    #[test]
    fn minimap_frames_draw_then_redraw_then_stop_after_finish() {
        let map = OpenMinimap::new(1000);
        let first = map.frame(0, 100).expect("first frame draws");
        assert!(
            !first.contains("\x1b[1A"),
            "first frame must not move the cursor up: {first:?}"
        );
        let second = map.frame(500, 100).expect("second frame redraws");
        assert!(
            second.starts_with("\r\x1b[1A"),
            "redraw must reclaim its two lines: {second:?}"
        );
        map.finish();
        assert_eq!(
            map.frame(900, 50),
            None,
            "a finished minimap must never draw again"
        );
    }

    #[test]
    fn caret_line_points_at_the_current_fetch() {
        assert_eq!(caret_line(0), "  ▼");
        assert_eq!(caret_line(3), "     ▼");
    }
}
