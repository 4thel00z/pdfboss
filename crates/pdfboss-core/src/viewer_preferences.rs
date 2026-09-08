//! The viewer preferences the catalog declares (ISO 32000-1 §12.2): how a
//! reader should present the document on the screen and in print, read
//! from the catalog's `/ViewerPreferences` dictionary (Table 150).

use crate::object::{Dict, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// The page mode a reader returns to on leaving full-screen mode
/// (`/NonFullScreenPageMode`), meaningful only when the catalog's
/// `/PageMode` is `FullScreen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NonFullScreenPageMode {
    /// Neither the outline nor the thumbnails are shown.
    #[default]
    UseNone,
    /// The document outline is shown.
    UseOutlines,
    /// The thumbnail images are shown.
    UseThumbs,
    /// The optional content group panel is shown.
    UseOC,
}

impl NonFullScreenPageMode {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "UseNone" => Some(Self::UseNone),
            "UseOutlines" => Some(Self::UseOutlines),
            "UseThumbs" => Some(Self::UseThumbs),
            "UseOC" => Some(Self::UseOC),
            _ => None,
        }
    }
}

/// The predominant reading order of the text (`/Direction`), which places
/// pages shown side by side; it does not change the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// `L2R`: left to right.
    #[default]
    LeftToRight,
    /// `R2L`: right to left, vertical writing systems included.
    RightToLeft,
}

impl Direction {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "L2R" => Some(Self::LeftToRight),
            "R2L" => Some(Self::RightToLeft),
            _ => None,
        }
    }
}

/// A page boundary named by key (§14.11.2), as the view and print area
/// and clip entries name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageBoundary {
    MediaBox,
    #[default]
    CropBox,
    BleedBox,
    TrimBox,
    ArtBox,
}

impl PageBoundary {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "MediaBox" => Some(Self::MediaBox),
            "CropBox" => Some(Self::CropBox),
            "BleedBox" => Some(Self::BleedBox),
            "TrimBox" => Some(Self::TrimBox),
            "ArtBox" => Some(Self::ArtBox),
            _ => None,
        }
    }
}

/// The page scaling a print dialog starts with (`/PrintScaling`); a value
/// the standard does not know reads as `AppDefault`, as it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintScaling {
    /// No page scaling.
    None,
    /// The reader's own default.
    #[default]
    AppDefault,
}

impl PrintScaling {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "None" => Some(Self::None),
            "AppDefault" => Some(Self::AppDefault),
            _ => None,
        }
    }
}

/// The paper handling a print dialog starts with (`/Duplex`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Duplex {
    /// Single-sided.
    Simplex,
    /// Two-sided, flipped on the short edge.
    DuplexFlipShortEdge,
    /// Two-sided, flipped on the long edge.
    DuplexFlipLongEdge,
}

impl Duplex {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "Simplex" => Some(Self::Simplex),
            "DuplexFlipShortEdge" => Some(Self::DuplexFlipShortEdge),
            "DuplexFlipLongEdge" => Some(Self::DuplexFlipLongEdge),
            _ => None,
        }
    }
}

/// The catalog's viewer preferences (ISO 32000-1 §12.2, Table 150). Every
/// entry is optional; a missing one holds the default the table gives, and
/// a `None` where the table leaves the default to the reader.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewerPreferences {
    /// `/HideToolbar`: hide the reader's tool bars.
    pub hide_toolbar: bool,
    /// `/HideMenubar`: hide the reader's menu bar.
    pub hide_menubar: bool,
    /// `/HideWindowUI`: hide the window's scroll bars and controls.
    pub hide_window_ui: bool,
    /// `/FitWindow`: size the window to the first page.
    pub fit_window: bool,
    /// `/CenterWindow`: center the window on the screen.
    pub center_window: bool,
    /// `/DisplayDocTitle`: title the window by the information dictionary's
    /// `/Title` rather than the file name.
    pub display_doc_title: bool,
    /// `/NonFullScreenPageMode`, `UseNone` by default.
    pub non_full_screen_page_mode: NonFullScreenPageMode,
    /// `/Direction`, left to right by default.
    pub direction: Direction,
    /// `/ViewArea`: the boundary shown on the screen, `CropBox` by default.
    pub view_area: PageBoundary,
    /// `/ViewClip`: the boundary the screen clips to, `CropBox` by default.
    pub view_clip: PageBoundary,
    /// `/PrintArea`: the boundary printed, `CropBox` by default.
    pub print_area: PageBoundary,
    /// `/PrintClip`: the boundary printing clips to, `CropBox` by default.
    pub print_clip: PageBoundary,
    /// `/PrintScaling`, `AppDefault` by default and for a value the standard
    /// does not know.
    pub print_scaling: PrintScaling,
    /// `/Duplex`; `None` when absent or not one of the three values, the
    /// reader deciding.
    pub duplex: Option<Duplex>,
    /// `/PickTrayByPDFSize`; `None` when absent, the reader deciding.
    pub pick_tray_by_pdf_size: Option<bool>,
    /// `/PrintPageRange`: the sub-ranges to print as (first, last) pairs of
    /// page numbers counted from 1; empty when absent or when no complete
    /// pair of positive integers is given.
    pub print_page_range: Vec<(u32, u32)>,
    /// `/NumCopies`; `None` when absent or not a positive integer, the
    /// reader deciding.
    pub num_copies: Option<u32>,
}

/// The catalog's `/ViewerPreferences` (ISO 32000-1 §12.2), `None` when the
/// catalog has none; a present dictionary reads every entry of Table 150
/// with the table's defaults for the missing ones, indirect values
/// followed, and a name the standard does not know left at the default.
///
/// Covers ISO 32000-1 §12.2.
pub async fn viewer_preferences_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Option<ViewerPreferences> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let dict = resolved_dict(src, catalog.get("ViewerPreferences")?).await?;
    let entries = Entries { src, dict: &dict };
    Some(ViewerPreferences {
        hide_toolbar: entries.flag("HideToolbar").await.unwrap_or(false),
        hide_menubar: entries.flag("HideMenubar").await.unwrap_or(false),
        hide_window_ui: entries.flag("HideWindowUI").await.unwrap_or(false),
        fit_window: entries.flag("FitWindow").await.unwrap_or(false),
        center_window: entries.flag("CenterWindow").await.unwrap_or(false),
        display_doc_title: entries.flag("DisplayDocTitle").await.unwrap_or(false),
        non_full_screen_page_mode: entries
            .named("NonFullScreenPageMode", NonFullScreenPageMode::from_name)
            .await
            .unwrap_or_default(),
        direction: entries
            .named("Direction", Direction::from_name)
            .await
            .unwrap_or_default(),
        view_area: entries
            .named("ViewArea", PageBoundary::from_name)
            .await
            .unwrap_or_default(),
        view_clip: entries
            .named("ViewClip", PageBoundary::from_name)
            .await
            .unwrap_or_default(),
        print_area: entries
            .named("PrintArea", PageBoundary::from_name)
            .await
            .unwrap_or_default(),
        print_clip: entries
            .named("PrintClip", PageBoundary::from_name)
            .await
            .unwrap_or_default(),
        print_scaling: entries
            .named("PrintScaling", PrintScaling::from_name)
            .await
            .unwrap_or_default(),
        duplex: entries.named("Duplex", Duplex::from_name).await,
        pick_tray_by_pdf_size: entries.flag("PickTrayByPDFSize").await,
        print_page_range: page_ranges(entries.value("PrintPageRange").await.as_ref()),
        num_copies: entries
            .value("NumCopies")
            .await
            .and_then(|value| value.as_int())
            .and_then(|copies| u32::try_from(copies).ok())
            .filter(|copies| *copies > 0),
    })
}

/// The (first, last) pairs of a `/PrintPageRange` array: complete pairs of
/// positive integers, a trailing odd element and a pair with anything else
/// in it dropped.
fn page_ranges(value: Option<&Object>) -> Vec<(u32, u32)> {
    let Some(items) = value.and_then(Object::as_array) else {
        return Vec::new();
    };
    items
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|[first, last]| {
            let first = u32::try_from(first.as_int()?).ok()?;
            let last = u32::try_from(last.as_int()?).ok()?;
            (first > 0 && last > 0).then_some((first, last))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Direction, Duplex, NonFullScreenPageMode, PageBoundary, PrintScaling, ViewerPreferences,
    };
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

    fn doc(catalog_extra: &str, objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).unwrap()
    }

    /// Every entry of Table 150 set, two of them through indirect objects
    /// and the dictionary itself indirect.
    // Covers ISO 32000-1 §12.2.
    #[test]
    fn every_viewer_preference_is_read() {
        let found = doc(
            "/ViewerPreferences 4 0 R",
            &[
                (
                    4,
                    "<< /HideToolbar true /HideMenubar true /HideWindowUI true /FitWindow true \
                     /CenterWindow true /DisplayDocTitle true /NonFullScreenPageMode /UseOutlines \
                     /Direction /R2L /ViewArea /MediaBox /ViewClip /BleedBox /PrintArea /TrimBox \
                     /PrintClip /ArtBox /PrintScaling /None /Duplex /DuplexFlipLongEdge \
                     /PickTrayByPDFSize 5 0 R /PrintPageRange [1 3 7 7] /NumCopies 6 0 R >>",
                ),
                (5, "false"),
                (6, "2"),
            ],
        )
        .viewer_preferences();
        assert_eq!(
            found,
            Some(ViewerPreferences {
                hide_toolbar: true,
                hide_menubar: true,
                hide_window_ui: true,
                fit_window: true,
                center_window: true,
                display_doc_title: true,
                non_full_screen_page_mode: NonFullScreenPageMode::UseOutlines,
                direction: Direction::RightToLeft,
                view_area: PageBoundary::MediaBox,
                view_clip: PageBoundary::BleedBox,
                print_area: PageBoundary::TrimBox,
                print_clip: PageBoundary::ArtBox,
                print_scaling: PrintScaling::None,
                duplex: Some(Duplex::DuplexFlipLongEdge),
                pick_tray_by_pdf_size: Some(false),
                print_page_range: vec![(1, 3), (7, 7)],
                num_copies: Some(2),
            })
        );
    }

    /// An empty dictionary holds the table's defaults, a catalog without
    /// one has no preferences, and values the standard does not know (a
    /// name it does not list, an odd page range, a zero copy count) fall
    /// back to the defaults entry by entry.
    // Covers ISO 32000-1 §12.2.
    #[test]
    fn missing_and_unknown_viewer_preferences_take_the_defaults() {
        assert_eq!(
            doc("/ViewerPreferences << >>", &[]).viewer_preferences(),
            Some(ViewerPreferences::default())
        );
        assert_eq!(doc("", &[]).viewer_preferences(), None);
        assert_eq!(doc("/ViewerPreferences 7", &[]).viewer_preferences(), None);
        let odd = doc(
            "/ViewerPreferences << /Direction /Down /PrintScaling /Bogus /Duplex /Both \
             /ViewArea /Page /PrintPageRange [1 3 5] /NumCopies 0 /HideToolbar /yes >>",
            &[],
        )
        .viewer_preferences()
        .unwrap();
        assert_eq!(
            odd,
            ViewerPreferences {
                print_page_range: vec![(1, 3)],
                ..ViewerPreferences::default()
            }
        );
    }
}
