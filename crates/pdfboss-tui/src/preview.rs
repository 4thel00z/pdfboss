//! Page preview: a rasterized page painted with `▀` half-blocks — the
//! upper pixel of each terminal cell is the foreground color, the lower
//! pixel the background color, two vertical pixels per cell. Rendering
//! happens off the event loop; this module is pure state and math.

use std::sync::Arc;

use pdfboss_render::Pixmap;
use ratatui::style::Color;

/// Spinner frames shown while a render is in flight.
pub const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
/// Resize debounce in 100 ms ticks (~200 ms).
pub const RESIZE_DEBOUNCE_TICKS: u8 = 2;

/// A finished render plus the file bytes fetched for it (cached so later
/// renders skip the fetch).
#[derive(Debug)]
pub struct PreviewFrame {
    pub file_bytes: Arc<Vec<u8>>,
    pub pixmap: Pixmap,
    /// One-line summary of anything the render had to drop, so a preview
    /// that came out blank because pdfboss could not read the page says so
    /// instead of looking like an empty page.
    pub notice: Option<String>,
}

/// Preview pane model.
pub struct PreviewState {
    /// Whether the preview replaces the inspector (`p`).
    pub active: bool,
    pub page: Option<usize>,
    pub pixmap: Option<Pixmap>,
    pub rendering: bool,
    pub spinner_frame: usize,
    pub generation: u64,
    pub file_bytes: Option<Arc<Vec<u8>>>,
    /// Ticks until a resize-deferred re-render fires.
    pub debounce: Option<u8>,
    pub error: Option<String>,
    /// What the last accepted render had to drop, if anything (see
    /// [`PreviewFrame::notice`]).
    pub notice: Option<String>,
}

impl PreviewState {
    pub fn new() -> PreviewState {
        PreviewState {
            active: false,
            page: None,
            pixmap: None,
            rendering: false,
            spinner_frame: 0,
            generation: 0,
            file_bytes: None,
            debounce: None,
            error: None,
            notice: None,
        }
    }

    /// Marks a render in flight for `page`; returns its generation.
    pub fn start_render(&mut self, page: usize) -> u64 {
        self.generation += 1;
        self.page = Some(page);
        self.rendering = true;
        self.error = None;
        self.notice = None;
        self.debounce = None;
        self.generation
    }

    /// Applies a finished render; stale generations are dropped. Returns
    /// whether the result was accepted.
    ///
    /// The whole-file bytes are cached *before* the generation check: they
    /// are generation-independent (the same file backs every render of
    /// this document), so even a superseded render's bytes are worth
    /// keeping — dropping them here would force the next render to
    /// re-fetch the entire file. Only the pixmap/error handling stays
    /// gated on the generation matching.
    pub fn apply_ready(&mut self, generation: u64, result: Result<PreviewFrame, String>) -> bool {
        if let Ok(frame) = &result {
            self.file_bytes = Some(Arc::clone(&frame.file_bytes));
        }
        if generation != self.generation {
            return false;
        }
        self.rendering = false;
        match result {
            Ok(frame) => {
                self.pixmap = Some(frame.pixmap);
                self.error = None;
                self.notice = frame.notice;
            }
            Err(message) => self.error = Some(message),
        }
        true
    }

    /// 100 ms heartbeat: advances the spinner and counts the resize
    /// debounce down. Returns true when a deferred re-render should fire.
    pub fn tick(&mut self) -> bool {
        if self.rendering {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER.len();
        }
        match self.debounce {
            Some(0) | None => {
                self.debounce = None;
                false
            }
            Some(1) => {
                self.debounce = None;
                self.active
            }
            Some(remaining) => {
                self.debounce = Some(remaining - 1);
                false
            }
        }
    }
}

impl Default for PreviewState {
    fn default() -> PreviewState {
        PreviewState::new()
    }
}

/// The scale that fits a `page_w x page_h` point page inside a
/// `max_w x max_h` pixel budget, preserving aspect ratio.
pub fn fit_scale(page_w: f32, page_h: f32, max_w: u32, max_h: u32) -> f32 {
    if !(page_w.is_finite() && page_h.is_finite()) || page_w <= 0.0 || page_h <= 0.0 {
        return 1.0;
    }
    let horizontal = max_w as f32 / page_w;
    let vertical = max_h as f32 / page_h;
    horizontal.min(vertical).max(0.001)
}

/// RGBA (straight alpha) composited over the white page background.
fn blend_over_white(rgba: [u8; 4]) -> Color {
    let alpha = rgba[3] as u32;
    let channel = |value: u8| -> u8 { ((value as u32 * alpha + 255 * (255 - alpha)) / 255) as u8 };
    Color::Rgb(channel(rgba[0]), channel(rgba[1]), channel(rgba[2]))
}

fn pixel(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
    if x >= pix.width || y >= pix.height {
        return [255, 255, 255, 255];
    }
    let index = ((y * pix.width + x) * 4) as usize;
    [
        pix.data[index],
        pix.data[index + 1],
        pix.data[index + 2],
        pix.data[index + 3],
    ]
}

/// The `(foreground, background)` of terminal cell `(x, row)`: pixel rows
/// `2*row` (upper, fg of `▀`) and `2*row + 1` (lower, bg).
pub fn cell_colors(pix: &Pixmap, x: u32, row: u32) -> (Color, Color) {
    (
        blend_over_white(pixel(pix, x, row * 2)),
        blend_over_white(pixel(pix, x, row * 2 + 1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn two_by_two() -> Pixmap {
        // Row 0: red, green; row 1: blue, transparent.
        Pixmap {
            width: 2,
            height: 2,
            data: vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 0, 0, 0, 0,
            ],
        }
    }

    #[test]
    fn fit_scale_fits_both_axes() {
        assert_eq!(fit_scale(100.0, 100.0, 200, 50), 0.5);
        assert_eq!(fit_scale(100.0, 100.0, 50, 200), 0.5);
        assert_eq!(fit_scale(612.0, 792.0, 612, 792), 1.0);
        assert_eq!(fit_scale(0.0, 100.0, 50, 50), 1.0, "degenerate page");
        assert!(
            fit_scale(1_000_000.0, 1.0, 10, 10) >= 0.001,
            "clamped floor"
        );
    }

    #[test]
    fn cell_colors_pack_two_rows_per_cell() {
        let pix = two_by_two();
        assert_eq!(
            cell_colors(&pix, 0, 0),
            (Color::Rgb(255, 0, 0), Color::Rgb(0, 0, 255))
        );
        // Transparent blends to white; out-of-range pixels are white.
        assert_eq!(
            cell_colors(&pix, 1, 0),
            (Color::Rgb(0, 255, 0), Color::Rgb(255, 255, 255))
        );
        assert_eq!(
            cell_colors(&pix, 5, 9),
            (Color::Rgb(255, 255, 255), Color::Rgb(255, 255, 255))
        );
    }

    #[test]
    fn start_render_bumps_generation_and_spins() {
        let mut preview = PreviewState::new();
        let first = preview.start_render(0);
        let second = preview.start_render(0);
        assert!(second > first);
        assert!(preview.rendering);
        let before = preview.spinner_frame;
        assert!(!preview.tick());
        assert_ne!(
            preview.spinner_frame, before,
            "spinner advances while rendering"
        );
    }

    #[test]
    fn apply_ready_ignores_stale_generations() {
        let mut preview = PreviewState::new();
        let stale = preview.start_render(0);
        let current = preview.start_render(0);
        let frame = PreviewFrame {
            file_bytes: Arc::new(vec![1, 2, 3]),
            pixmap: two_by_two(),
            notice: None,
        };
        assert!(!preview.apply_ready(stale, Ok(frame)));
        assert!(preview.rendering, "stale result leaves the spinner on");
        let frame = PreviewFrame {
            file_bytes: Arc::new(vec![1, 2, 3]),
            pixmap: two_by_two(),
            notice: None,
        };
        assert!(preview.apply_ready(current, Ok(frame)));
        assert!(!preview.rendering);
        assert!(preview.pixmap.is_some());
        assert!(preview.file_bytes.is_some(), "bytes cached for re-renders");
        assert!(preview.apply_ready(current, Err("boom".to_string())));
        assert_eq!(preview.error.as_deref(), Some("boom"));
    }

    #[test]
    fn stale_frame_still_caches_file_bytes() {
        let mut preview = PreviewState::new();
        let stale = preview.start_render(0);
        let _current = preview.start_render(0);
        let frame = PreviewFrame {
            file_bytes: Arc::new(vec![9, 9, 9]),
            pixmap: two_by_two(),
            notice: None,
        };
        assert!(
            !preview.apply_ready(stale, Ok(frame)),
            "stale generation is still rejected"
        );
        assert!(
            preview.pixmap.is_none(),
            "stale pixmap must not be installed"
        );
        assert!(
            preview.file_bytes.is_some(),
            "whole-file bytes are generation-independent and must be cached \
             even from a superseded render, so the next render skips re-fetching"
        );
    }

    #[test]
    fn debounce_counts_down_to_render_request() {
        let mut preview = PreviewState::new();
        preview.active = true;
        preview.debounce = Some(RESIZE_DEBOUNCE_TICKS);
        assert!(!preview.tick());
        assert!(preview.tick(), "second tick fires the deferred render");
        assert_eq!(preview.debounce, None);
        assert!(!preview.tick(), "no further fires");
    }
}
