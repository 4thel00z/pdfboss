//! Measurement properties (ISO 32000-1 §12.9): a page's `/VP` viewports,
//! each a rectangle with a measure dictionary that maps user space to
//! real-world distances, read as data with the tables' defaults filled in.

use crate::document::Page;
use crate::geom::Rect;
use crate::object::{Dict, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// A viewport (ISO 32000-1 §12.9, Table 260): a page rectangle with its own
/// measurement scale. Where viewports overlap, the last one in the array
/// whose box contains a point applies to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    /// `/BBox`: the rectangle in default user space, normalized.
    pub bbox: Rect,
    /// `/Name`: a descriptive title.
    pub name: Option<String>,
    /// `/Measure`: the units of the viewport's coordinate system; `None`
    /// without one.
    pub measure: Option<Measure>,
}

/// A measure dictionary (ISO 32000-1 §12.9, Tables 261 and 262): how
/// distances, areas and angles in a viewport convert to real-world units.
#[derive(Debug, Clone, PartialEq)]
pub struct Measure {
    /// `/Subtype`: `RL`, rectilinear, the one kind ISO 32000-1 defines and
    /// the default; any other name is kept as written.
    pub subtype: String,
    /// `/R`: the scale ratio as text, `1in = 0.1 mi` style.
    pub scale_ratio: Option<String>,
    /// `/X`: the number formats for x distances, a chain from the coarsest
    /// unit to the finest.
    pub x: Vec<NumberFormat>,
    /// `/Y`: the formats for y distances, empty when they share `x`'s.
    pub y: Vec<NumberFormat>,
    /// `/D`: the formats for distances.
    pub distance: Vec<NumberFormat>,
    /// `/A`: the formats for areas.
    pub area: Vec<NumberFormat>,
    /// `/T`: the formats for angles.
    pub angle: Vec<NumberFormat>,
    /// `/S`: the formats for slopes.
    pub slope: Vec<NumberFormat>,
    /// `/O`: the origin of the measurement coordinate system in default user
    /// space, `(0, 0)` when absent.
    pub origin: (f64, f64),
    /// `/CYX`: the factor that converts y units to x units when the two
    /// differ.
    pub y_to_x: Option<f64>,
}

/// A number format (ISO 32000-1 §12.9, Table 263): one unit of a
/// measurement chain and how its value is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct NumberFormat {
    /// `/U`: the unit label.
    pub unit: String,
    /// `/C`: the factor that converts the previous unit of the chain, user
    /// space units for the first, into this one.
    pub conversion: f64,
    /// `/F`: how the fractional part is shown, decimal when absent.
    pub fraction: FractionFormat,
    /// `/D`: the places of a decimal or the denominator of a fraction, 100
    /// when absent.
    pub precision: u32,
    /// `/FD`: whether a fraction keeps the denominator `precision` names
    /// instead of being reduced.
    pub fixed_denominator: bool,
    /// `/RT`: the text between orders of thousands, a comma when absent.
    pub thousands: String,
    /// `/RD`: the decimal point text, a period when absent.
    pub radix: String,
    /// `/PS`: the text before the label, one space when absent.
    pub prefix_spacing: String,
    /// `/SS`: the text after the label, one space when absent.
    pub suffix_spacing: String,
    /// `/O`: where the label goes, after the value when absent.
    pub label: LabelPosition,
}

/// `/F`: how a number format shows the fractional part of a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FractionFormat {
    /// `/D`: as a decimal to `precision` places, the default.
    #[default]
    Decimal,
    /// `/F`: as a fraction over the denominator `precision` names.
    Fraction,
    /// `/R`: rounded to a whole number.
    Round,
    /// `/T`: truncated to a whole number.
    Truncate,
}

impl FractionFormat {
    /// The format `/F` names; `None` for a name the table does not list.
    pub fn from_name(name: &str) -> Option<FractionFormat> {
        match name {
            "D" => Some(FractionFormat::Decimal),
            "F" => Some(FractionFormat::Fraction),
            "R" => Some(FractionFormat::Round),
            "T" => Some(FractionFormat::Truncate),
            _ => None,
        }
    }
}

/// `/O`: whether a number format's label follows or precedes the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LabelPosition {
    /// `/S`: after the value, the default.
    #[default]
    Suffix,
    /// `/P`: before the value.
    Prefix,
}

impl LabelPosition {
    /// The position `/O` names; `None` for any other name.
    pub fn from_name(name: &str) -> Option<LabelPosition> {
        match name {
            "S" => Some(LabelPosition::Suffix),
            "P" => Some(LabelPosition::Prefix),
            _ => None,
        }
    }
}

/// The viewports of `page`'s `/VP` array, in order: empty without one. A
/// viewport that is no dictionary, or whose `/BBox` is no array of four
/// finite numbers, is skipped; a `/Measure` that is no dictionary reads as
/// absent.
///
/// Covers ISO 32000-1 §12.9.
pub async fn viewports_with<S: AsyncObjectSource>(src: &S, page: &Page) -> Vec<Viewport> {
    let Some(entry) = page.dict().get("VP") else {
        return Vec::new();
    };
    let Ok(Object::Array(entries)) = src.resolve(entry).await else {
        return Vec::new();
    };
    let mut viewports = Vec::new();
    for entry in &entries {
        let Some(dict) = resolved_dict(src, entry).await else {
            continue;
        };
        let viewport = Entries { src, dict: &dict };
        let bbox = match numbers_of(src, viewport.value("BBox").await)
            .await
            .as_deref()
        {
            Some(&[x0, y0, x1, y1]) => {
                Rect::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32).normalize()
            }
            _ => continue,
        };
        let measure = match dict.get("Measure") {
            Some(value) => match resolved_dict(src, value).await {
                Some(measure) => Some(measure_from(src, &measure).await),
                None => None,
            },
            None => None,
        };
        viewports.push(Viewport {
            bbox,
            name: viewport.text("Name").await,
            measure,
        });
    }
    viewports
}

/// Tables 261 and 262 with defaults: a subtype other than `RL` keeps its
/// name and whatever rectilinear entries it carries.
///
/// Covers ISO 32000-1 §12.9.
pub async fn measure_from<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Measure {
    let entries = Entries { src, dict };
    let origin = match numbers_of(src, entries.value("O").await).await.as_deref() {
        Some(&[x, y]) => (x, y),
        _ => (0.0, 0.0),
    };
    Measure {
        subtype: entries
            .value("Subtype")
            .await
            .as_ref()
            .and_then(Object::as_name)
            .map(|name| name.0.clone())
            .unwrap_or_else(|| "RL".into()),
        scale_ratio: entries.text("R").await,
        x: formats_of(src, dict, "X").await,
        y: formats_of(src, dict, "Y").await,
        distance: formats_of(src, dict, "D").await,
        area: formats_of(src, dict, "A").await,
        angle: formats_of(src, dict, "T").await,
        slope: formats_of(src, dict, "S").await,
        origin,
        y_to_x: entries.value("CYX").await.and_then(|value| value.as_f64()),
    }
}

/// The number formats under `key`, in order; a format that is no
/// dictionary, or lacks the required `/U` label or `/C` factor, is skipped.
///
/// Covers ISO 32000-1 §12.9.
async fn formats_of<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Vec<NumberFormat> {
    let Some(value) = Entries { src, dict }.value(key).await else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut formats = Vec::new();
    for item in items {
        let Some(dict) = resolved_dict(src, item).await else {
            continue;
        };
        let format = Entries { src, dict: &dict };
        let Some(unit) = format.text("U").await else {
            continue;
        };
        let Some(conversion) = format.value("C").await.and_then(|value| value.as_f64()) else {
            continue;
        };
        formats.push(NumberFormat {
            unit,
            conversion,
            fraction: format
                .named("F", FractionFormat::from_name)
                .await
                .unwrap_or_default(),
            precision: format
                .value("D")
                .await
                .and_then(|value| value.as_int())
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(100),
            fixed_denominator: format.flag("FD").await.unwrap_or(false),
            thousands: format.text("RT").await.unwrap_or_else(|| ",".into()),
            radix: format.text("RD").await.unwrap_or_else(|| ".".into()),
            prefix_spacing: format.text("PS").await.unwrap_or_else(|| " ".into()),
            suffix_spacing: format.text("SS").await.unwrap_or_else(|| " ".into()),
            label: format
                .named("O", LabelPosition::from_name)
                .await
                .unwrap_or_default(),
        });
    }
    formats
}

/// The finite numbers of an array, each resolved; `None` when the value is
/// no array or any item is no finite number.
async fn numbers_of<S: AsyncObjectSource>(src: &S, value: Option<Object>) -> Option<Vec<f64>> {
    let value = value?;
    let items = value.as_array()?;
    let mut numbers = Vec::with_capacity(items.len());
    for item in items {
        let number = src.resolve(item).await.ok()?.as_f64()?;
        if !number.is_finite() {
            return None;
        }
        numbers.push(number);
    }
    Some(numbers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::geom::Rect;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose page carries `page_extra`; object 20 is a
    /// viewport in the manner of the clause's example, reachable by
    /// reference.
    fn doc_with(page_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page_extra} >>"),
        );
        b.object(
            20,
            "<< /Type /Viewport /BBox [0 0 612 792] /Name (Map) \
             /Measure << /Type /Measure /Subtype /RL /R (1in = 0.1 mi) \
             /X [ << /Type /NumberFormat /U (mi) /C 0.00139 /D 100000 >> ] \
             /D [ << /U (mi) /C 1 >> << /U (feet) /C 5280 /F /F /D 8 >> ] \
             /A [ << /U (acres) /C 640 >> ] >> >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    fn viewports(page_extra: &str) -> Vec<Viewport> {
        let doc = doc_with(page_extra);
        doc.viewports(&doc.page(0).unwrap())
    }

    /// The `/VP` array reads in order, by reference or written directly:
    /// the bounding box (normalized), the name and the rectilinear measure
    /// with its number formats, absent entries at Table 263's defaults.
    // Covers ISO 32000-1 §12.9.
    #[test]
    fn reads_viewports_with_their_measures_and_the_tables_defaults() {
        let viewports = viewports("/VP [ 20 0 R << /BBox [100 10 10 100] >> ]");
        assert_eq!(viewports.len(), 2);
        let map = &viewports[0];
        assert_eq!(map.bbox, Rect::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(map.name.as_deref(), Some("Map"));
        let measure = map.measure.as_ref().unwrap();
        assert_eq!(measure.subtype, "RL");
        assert_eq!(measure.scale_ratio.as_deref(), Some("1in = 0.1 mi"));
        assert_eq!(
            measure.x,
            [NumberFormat {
                unit: "mi".into(),
                conversion: 0.00139,
                fraction: FractionFormat::Decimal,
                precision: 100000,
                fixed_denominator: false,
                thousands: ",".into(),
                radix: ".".into(),
                prefix_spacing: " ".into(),
                suffix_spacing: " ".into(),
                label: LabelPosition::Suffix,
            }]
        );
        assert_eq!(measure.distance.len(), 2);
        assert_eq!(measure.distance[0].unit, "mi");
        assert_eq!(measure.distance[0].conversion, 1.0);
        assert_eq!(measure.distance[1].unit, "feet");
        assert_eq!(measure.distance[1].conversion, 5280.0);
        assert_eq!(measure.distance[1].fraction, FractionFormat::Fraction);
        assert_eq!(measure.distance[1].precision, 8);
        assert_eq!(measure.area.len(), 1);
        assert_eq!(measure.area[0].unit, "acres");
        assert!(measure.y.is_empty());
        assert!(measure.angle.is_empty());
        assert!(measure.slope.is_empty());
        assert_eq!(measure.origin, (0.0, 0.0));
        assert_eq!(measure.y_to_x, None);
        let plain = &viewports[1];
        assert_eq!(plain.bbox, Rect::new(10.0, 10.0, 100.0, 100.0));
        assert_eq!(plain.name, None);
        assert_eq!(plain.measure, None);
    }

    /// The optional measure entries: a `/Y` chain, an origin, the y-to-x
    /// factor, a subtype other than RL kept by name, and the number
    /// format's spacing, separator, position and fraction entries; a format
    /// that is no dictionary or lacks `/U` or `/C` is skipped.
    // Covers ISO 32000-1 §12.9.
    #[test]
    fn reads_the_optional_measure_entries() {
        let viewports = viewports(
            "/VP [ << /BBox [0 0 1 1] /Measure << /Subtype /GEO /Y [ << /U (m) /C 2 >> ] \
             /O [5 6] /CYX 2 /T [ << /U (deg) /C 1 /O /P /RT (.) /RD (,) /PS () /SS () \
             /FD true /F /R >> ] /S [ 7 << /U (%) >> << /C 1 >> ] /D [ << /U (m) /C 1 /F /Weird >> ] >> >> ]",
        );
        let measure = viewports[0].measure.as_ref().unwrap();
        assert_eq!(measure.subtype, "GEO");
        assert_eq!(measure.scale_ratio, None);
        assert_eq!(measure.y.len(), 1);
        assert_eq!(measure.y[0].unit, "m");
        assert_eq!(measure.y[0].precision, 100);
        assert_eq!(measure.origin, (5.0, 6.0));
        assert_eq!(measure.y_to_x, Some(2.0));
        let angle = &measure.angle[0];
        assert_eq!(angle.label, LabelPosition::Prefix);
        assert_eq!(angle.thousands, ".");
        assert_eq!(angle.radix, ",");
        assert_eq!(angle.prefix_spacing, "");
        assert_eq!(angle.suffix_spacing, "");
        assert!(angle.fixed_denominator);
        assert_eq!(angle.fraction, FractionFormat::Round);
        assert!(measure.slope.is_empty());
        assert_eq!(measure.distance[0].fraction, FractionFormat::Decimal);
    }

    /// A page without `/VP`, or whose `/VP` is no array, has no viewports;
    /// a viewport that is no dictionary or has no four-number `/BBox` is
    /// skipped, and a `/Measure` that is no dictionary reads as absent.
    // Covers ISO 32000-1 §12.9.
    #[test]
    fn missing_or_malformed_viewports_read_as_none() {
        assert!(viewports("").is_empty());
        assert!(viewports("/VP 5").is_empty());
        let odd = viewports(
            "/VP [ 7 << /Name (no box) >> << /BBox [0 0 1] >> << /BBox [0 0 5 5] /Measure 3 >> ]",
        );
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0].bbox, Rect::new(0.0, 0.0, 5.0, 5.0));
        assert_eq!(odd[0].measure, None);
    }
}
