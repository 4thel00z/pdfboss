//! Presentations (ISO 32000-1 §12.4.4): a page's display duration and its
//! transition dictionary, read as data with the table's defaults filled in.

use crate::document::Page;
use crate::object::{Dict, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// The transition styles of ISO 32000-1 Table 162: how a presentation
/// reveals the page it moves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransitionStyle {
    /// Two lines sweep across the screen, along `Dimension` and `Motion`.
    Split,
    /// Evenly spaced lines sweep in the same direction, along `Dimension`.
    Blinds,
    /// A rectangle sweeps inward from the edges or outward from the centre.
    Box,
    /// One line sweeps from one edge to the other, in `TransitionDirection`.
    Wipe,
    /// The old page dissolves into the new one.
    Dissolve,
    /// A dissolve that sweeps across the page as a band.
    Glitter,
    /// `/R`: the new page replaces the old one with no effect, the default.
    #[default]
    Replace,
    /// PDF 1.5: changes fly in or out, scaled from `Transition::scale`.
    Fly,
    /// PDF 1.5: the new page pushes the old one off the screen.
    Push,
    /// PDF 1.5: the new page slides over the old one.
    Cover,
    /// PDF 1.5: the old page slides off, uncovering the new one.
    Uncover,
    /// PDF 1.5: the new page fades in through the old one.
    Fade,
}

impl TransitionStyle {
    /// The style `/S` names; `None` for a name the table does not list.
    pub fn from_name(name: &str) -> Option<TransitionStyle> {
        Some(match name {
            "Split" => TransitionStyle::Split,
            "Blinds" => TransitionStyle::Blinds,
            "Box" => TransitionStyle::Box,
            "Wipe" => TransitionStyle::Wipe,
            "Dissolve" => TransitionStyle::Dissolve,
            "Glitter" => TransitionStyle::Glitter,
            "R" => TransitionStyle::Replace,
            "Fly" => TransitionStyle::Fly,
            "Push" => TransitionStyle::Push,
            "Cover" => TransitionStyle::Cover,
            "Uncover" => TransitionStyle::Uncover,
            "Fade" => TransitionStyle::Fade,
            _ => return None,
        })
    }
}

/// `/Dm`: the dimension a Split or Blinds transition moves along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dimension {
    /// `/H`, the default.
    #[default]
    Horizontal,
    /// `/V`.
    Vertical,
}

impl Dimension {
    /// The dimension `/Dm` names; `None` for any other name.
    pub fn from_name(name: &str) -> Option<Dimension> {
        match name {
            "H" => Some(Dimension::Horizontal),
            "V" => Some(Dimension::Vertical),
            _ => None,
        }
    }
}

/// `/M`: the direction of motion of a Split, Box or Fly transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    /// `/I`: inward from the edges of the page, the default.
    #[default]
    Inward,
    /// `/O`: outward from the centre of the page.
    Outward,
}

impl Motion {
    /// The motion `/M` names; `None` for any other name.
    pub fn from_name(name: &str) -> Option<Motion> {
        match name {
            "I" => Some(Motion::Inward),
            "O" => Some(Motion::Outward),
            _ => None,
        }
    }
}

/// `/Di`: the direction a Wipe, Glitter, Fly, Cover, Uncover or Push
/// transition moves in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionDirection {
    /// Degrees counterclockwise from left to right: 0, 90, 180, 270 or 315
    /// in the table, 0 being the default.
    Angle(u32),
    /// The name `None`, meaningful for a Fly transition whose scale is not 1.
    None,
}

impl Default for TransitionDirection {
    fn default() -> TransitionDirection {
        TransitionDirection::Angle(0)
    }
}

/// A transition dictionary (ISO 32000-1 §12.4.4, Table 162) with every
/// absent entry at its default.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// `/S`, `Replace` when absent or not a listed style.
    pub style: TransitionStyle,
    /// `/D`: the effect's duration in seconds, 1 when absent.
    pub duration: f64,
    /// `/Dm`, horizontal when absent.
    pub dimension: Dimension,
    /// `/M`, inward when absent.
    pub motion: Motion,
    /// `/Di`, left to right when absent.
    pub direction: TransitionDirection,
    /// `/SS`: the starting or ending scale of a Fly transition, 1 when absent.
    pub scale: f64,
    /// `/B`: whether a Fly transition's area is rectangular and opaque.
    pub opaque: bool,
}

impl Default for Transition {
    fn default() -> Transition {
        Transition {
            style: TransitionStyle::default(),
            duration: 1.0,
            dimension: Dimension::default(),
            motion: Motion::default(),
            direction: TransitionDirection::default(),
            scale: 1.0,
            opaque: false,
        }
    }
}

/// How a page takes part in a presentation (ISO 32000-1 §12.4.4): its
/// display duration and the transition that reveals it.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    /// `/Dur`: the seconds the page shows before the presentation advances
    /// on its own; `None` leaves the page waiting for the user.
    pub duration: Option<f64>,
    /// `/Trans`: the transition into this page; `None` without one.
    pub transition: Option<Transition>,
}

/// The presentation entries of `page`: `None` when it carries neither
/// `/Dur` nor a `/Trans` dictionary, so a plain page is no slide.
///
/// Covers ISO 32000-1 §12.4.4.
pub async fn presentation_with<S: AsyncObjectSource>(src: &S, page: &Page) -> Option<Presentation> {
    let dict = page.dict();
    let duration = match dict.get("Dur") {
        Some(value) => src
            .resolve(value)
            .await
            .ok()
            .and_then(|value| value.as_f64()),
        None => None,
    };
    let transition = match dict.get("Trans") {
        Some(value) => match resolved_dict(src, value).await {
            Some(trans) => Some(transition_from(src, &trans).await),
            None => None,
        },
        None => None,
    };
    if duration.is_none() && transition.is_none() {
        return None;
    }
    Some(Presentation {
        duration,
        transition,
    })
}

/// Table 162 with defaults: an entry that is absent, or a name the table
/// does not list, takes the default.
async fn transition_from<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Transition {
    let entries = Entries { src, dict };
    let defaults = Transition::default();
    let direction = match entries.value("Di").await {
        Some(Object::Name(name)) if name.0 == "None" => TransitionDirection::None,
        Some(value) => value
            .as_f64()
            .map(|degrees| TransitionDirection::Angle(degrees as u32))
            .unwrap_or_default(),
        None => TransitionDirection::default(),
    };
    Transition {
        style: entries
            .named("S", TransitionStyle::from_name)
            .await
            .unwrap_or_default(),
        duration: entries
            .value("D")
            .await
            .and_then(|value| value.as_f64())
            .unwrap_or(defaults.duration),
        dimension: entries
            .named("Dm", Dimension::from_name)
            .await
            .unwrap_or_default(),
        motion: entries
            .named("M", Motion::from_name)
            .await
            .unwrap_or_default(),
        direction,
        scale: entries
            .value("SS")
            .await
            .and_then(|value| value.as_f64())
            .unwrap_or(defaults.scale),
        opaque: entries.flag("B").await.unwrap_or(defaults.opaque),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose page carries `page_extra`.
    fn doc_with(page_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page_extra} >>"),
        );
        Document::load(b.build(1)).unwrap()
    }

    fn presentation(page_extra: &str) -> Option<Presentation> {
        let doc = doc_with(page_extra);
        doc.presentation(&doc.page(0).unwrap())
    }

    /// The clause's EXAMPLE: a five-second page reached through a
    /// 3.5-second vertical split moving outward; the entries the example
    /// leaves out take Table 162's defaults.
    // Covers ISO 32000-1 §12.4.4.
    #[test]
    fn reads_the_duration_and_the_transition_with_the_tables_defaults() {
        let shown =
            presentation("/Dur 5 /Trans << /Type /Trans /D 3.5 /S /Split /Dm /V /M /O >>").unwrap();
        assert_eq!(shown.duration, Some(5.0));
        assert_eq!(
            shown.transition,
            Some(Transition {
                style: TransitionStyle::Split,
                duration: 3.5,
                dimension: Dimension::Vertical,
                motion: Motion::Outward,
                direction: TransitionDirection::Angle(0),
                scale: 1.0,
                opaque: false,
            })
        );
        let fly = presentation("/Trans << /S /Fly /Di /None /SS 0.5 /B true >>").unwrap();
        assert_eq!(fly.duration, None);
        let fly = fly.transition.unwrap();
        assert_eq!(fly.style, TransitionStyle::Fly);
        assert_eq!(fly.direction, TransitionDirection::None);
        assert_eq!((fly.scale, fly.opaque), (0.5, true));
        let wipe = presentation("/Trans << /S /Wipe /Di 270 >>")
            .unwrap()
            .transition
            .unwrap();
        assert_eq!(wipe.style, TransitionStyle::Wipe);
        assert_eq!(wipe.direction, TransitionDirection::Angle(270));
        assert_eq!(
            presentation("/Trans << >>").unwrap().transition,
            Some(Transition::default())
        );
        assert_eq!(Transition::default().style, TransitionStyle::Replace);
        assert_eq!(Transition::default().duration, 1.0);
    }

    /// A page with neither entry is no slide; `/Dur` alone times the page
    /// without a transition; a `/Trans` that is no dictionary reads as no
    /// transition; an unknown style reads as the effect-free default.
    // Covers ISO 32000-1 §12.4.4.
    #[test]
    fn pages_without_the_entries_have_no_presentation_and_odd_values_fall_back() {
        assert_eq!(presentation(""), None);
        assert_eq!(
            presentation("/Dur 7"),
            Some(Presentation {
                duration: Some(7.0),
                transition: None,
            })
        );
        assert_eq!(presentation("/Trans 5"), None);
        assert_eq!(
            presentation("/Dur 2 /Trans 5"),
            Some(Presentation {
                duration: Some(2.0),
                transition: None,
            })
        );
        let odd = presentation("/Trans << /S /Sparkle /Dm /Q /M /Z /Di 45 >>")
            .unwrap()
            .transition
            .unwrap();
        assert_eq!(odd.style, TransitionStyle::Replace);
        assert_eq!(odd.dimension, Dimension::Horizontal);
        assert_eq!(odd.motion, Motion::Inward);
        assert_eq!(odd.direction, TransitionDirection::Angle(45));
    }
}
