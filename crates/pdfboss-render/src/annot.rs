//! Appearances for annotations that carry no `/AP` (ISO 32000-1 §12.5.6):
//! a form XObject built from the annotation's own entries, which the
//! executor runs exactly like a normal appearance stream, so the clip, the
//! resources chain and the opacity handling are the ones every `/AP` gets.

use pdfboss_core::{AsyncObjectSource, Dict, Name, Object, Stream};

use crate::executor::{dict_f32, floats_from, num_f32};

/// Bezier control distance that draws a quarter circle of radius 1.
const KAPPA: f32 = 0.552_284_8;

/// The name of the graphics state parameter dictionary carrying `/CA`.
const OPACITY_STATE: &str = "GS";

/// A form XObject painting the annotation from its own entries: `None` for
/// a subtype whose appearance is not synthesized, or for one whose entries
/// describe nothing to paint (no colour, or a zero-width border and no
/// interior).
///
/// Covers ISO 32000-1 §12.5.6.8.
pub(crate) async fn synthesized<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Option<Stream> {
    let subtype = annot.get_name("Subtype")?.0.clone();
    let rect = normalized(&floats_from(src, annot.get("Rect"), 4).await?);
    let paint = Paint::read(src, annot).await;
    let content = match subtype.as_str() {
        "Square" => paint.shape(inner(rect, &paint.differences), false)?,
        "Circle" => paint.shape(inner(rect, &paint.differences), true)?,
        _ => return None,
    };
    Some(form(rect, paint.opacity, content))
}

/// What the annotation's common entries say about how to paint it: the
/// `/C` stroke colour, the `/IC` interior colour, the border width and dash
/// pattern, the `/CA` opacity and the `/RD` rectangle differences.
struct Paint {
    stroke: Option<String>,
    fill: Option<String>,
    border: Border,
    opacity: Option<f32>,
    differences: Option<[f32; 4]>,
}

impl Paint {
    async fn read<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Paint {
        Paint {
            stroke: colour_op(src, annot, "C", true).await,
            fill: colour_op(src, annot, "IC", false).await,
            border: Border::read(src, annot).await,
            opacity: dict_f32(src, annot, "CA")
                .await
                .filter(|ca| (0.0..1.0).contains(ca)),
            differences: floats_from(src, annot.get("RD"), 4)
                .await
                .map(|rd| [rd[0], rd[1], rd[2], rd[3]]),
        }
    }

    /// The content that fills `rect` with the interior colour and strokes
    /// its outline, a rectangle or the inscribed ellipse, with the border
    /// drawn completely inside it (§12.5.4); `None` when nothing paints.
    ///
    /// Covers ISO 32000-1 §12.5.6.8.
    fn shape(&self, rect: [f32; 4], ellipse: bool) -> Option<String> {
        let stroke = self.stroke.as_deref().filter(|_| self.border.width > 0.0);
        let operator = match (self.fill.as_deref(), stroke) {
            (Some(_), Some(_)) => "B",
            (Some(_), None) => "f",
            (None, Some(_)) => "S",
            (None, None) => return None,
        };
        let inset = if stroke.is_some() {
            self.border.width / 2.0
        } else {
            0.0
        };
        let [x0, y0, x1, y1] = [
            rect[0] + inset,
            rect[1] + inset,
            rect[2] - inset,
            rect[3] - inset,
        ];
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let mut content = String::from("q\n");
        if self.opacity.is_some() {
            content.push_str(&format!("/{OPACITY_STATE} gs\n"));
        }
        if let Some(op) = stroke {
            content.push_str(op);
            content.push('\n');
            content.push_str(&format!("{} w\n", num(self.border.width)));
            if !self.border.dash.is_empty() {
                content.push_str(&format!("[{}] 0 d\n", nums(&self.border.dash)));
            }
        }
        if let Some(op) = &self.fill {
            content.push_str(op);
            content.push('\n');
        }
        if ellipse {
            content.push_str(&ellipse_path(x0, y0, x1, y1));
        } else {
            content.push_str(&format!(
                "{} {} {} {} re\n",
                num(x0),
                num(y0),
                num(x1 - x0),
                num(y1 - y0)
            ));
        }
        content.push_str(operator);
        content.push_str("\nQ\n");
        Some(content)
    }
}

/// The border width and dash pattern of §12.5.4: a `/BS` dictionary wins
/// over the `/Border` array; `/W` defaults to 1 and a `/S /D` style dashes
/// with `/D` (default `[3]`); `/Border` is `[h v w [dash]]`; with neither
/// entry the border is solid and 1 unit wide.
struct Border {
    width: f32,
    dash: Vec<f32>,
}

impl Border {
    async fn read<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Border {
        if let Some(bs) = annot.get("BS") {
            if let Ok(Object::Dict(bs)) = src.resolve(bs).await {
                let width = dict_f32(src, &bs, "W").await.unwrap_or(1.0).max(0.0);
                let dashed = bs.get_name("S").is_some_and(|s| s.0 == "D");
                let dash = match (dashed, bs.get("D")) {
                    (false, _) => Vec::new(),
                    (true, Some(d)) => dash_array(src, d).await,
                    (true, None) => vec![3.0],
                };
                return Border { width, dash };
            }
        }
        if let Some(border) = annot.get("Border") {
            if let Ok(Object::Array(items)) = src.resolve(border).await {
                let width = match items.get(2) {
                    Some(w) => num_f32(src, w).await.unwrap_or(1.0).max(0.0),
                    None => 1.0,
                };
                let dash = match items.get(3) {
                    Some(d) => dash_array(src, d).await,
                    None => Vec::new(),
                };
                return Border { width, dash };
            }
        }
        Border {
            width: 1.0,
            dash: Vec::new(),
        }
    }
}

/// A dash array's finite, non-negative entries; all zero or empty means
/// solid, so it comes back empty.
async fn dash_array<S: AsyncObjectSource>(src: &S, obj: &Object) -> Vec<f32> {
    let Ok(Object::Array(items)) = src.resolve(obj).await else {
        return Vec::new();
    };
    let mut dash = Vec::with_capacity(items.len());
    for item in &items {
        match num_f32(src, item).await {
            Some(v) if v >= 0.0 => dash.push(v),
            _ => return Vec::new(),
        }
    }
    if dash.iter().all(|v| *v == 0.0) {
        return Vec::new();
    }
    dash
}

/// A `/C` or `/IC` colour as the operator that selects it: 1, 3 or 4
/// components pick DeviceGray, DeviceRGB or DeviceCMYK; an empty array
/// means transparent and any other length is malformed, both `None`.
async fn colour_op<S: AsyncObjectSource>(
    src: &S,
    annot: &Dict,
    key: &str,
    stroking: bool,
) -> Option<String> {
    let Ok(Object::Array(items)) = src.resolve(annot.get(key)?).await else {
        return None;
    };
    let operator = match (items.len(), stroking) {
        (1, false) => "g",
        (1, true) => "G",
        (3, false) => "rg",
        (3, true) => "RG",
        (4, false) => "k",
        (4, true) => "K",
        _ => return None,
    };
    let mut components = Vec::with_capacity(items.len());
    for item in &items {
        components.push(num_f32(src, item).await?.clamp(0.0, 1.0));
    }
    Some(format!("{} {operator}", nums(&components)))
}

/// The rectangle `/RD` leaves inside `rect`: the differences are left, top,
/// right and bottom, each clamped so the result keeps a positive size.
fn inner(rect: [f32; 4], differences: &Option<[f32; 4]>) -> [f32; 4] {
    let Some([left, top, right, bottom]) = differences else {
        return rect;
    };
    let shrunk = [
        rect[0] + left.max(0.0),
        rect[1] + bottom.max(0.0),
        rect[2] - right.max(0.0),
        rect[3] - top.max(0.0),
    ];
    if shrunk[2] <= shrunk[0] || shrunk[3] <= shrunk[1] {
        return rect;
    }
    shrunk
}

/// The four Bezier arcs of the ellipse inscribed in the box, closed.
fn ellipse_path(x0: f32, y0: f32, x1: f32, y1: f32) -> String {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let curve = |p1: (f32, f32), p2: (f32, f32), p: (f32, f32)| {
        format!(
            "{} {} {} {} {} {} c\n",
            num(p1.0),
            num(p1.1),
            num(p2.0),
            num(p2.1),
            num(p.0),
            num(p.1)
        )
    };
    let mut path = format!("{} {} m\n", num(cx + rx), num(cy));
    path.push_str(&curve(
        (cx + rx, cy + ky),
        (cx + kx, cy + ry),
        (cx, cy + ry),
    ));
    path.push_str(&curve(
        (cx - kx, cy + ry),
        (cx - rx, cy + ky),
        (cx - rx, cy),
    ));
    path.push_str(&curve(
        (cx - rx, cy - ky),
        (cx - kx, cy - ry),
        (cx, cy - ry),
    ));
    path.push_str(&curve(
        (cx + kx, cy - ry),
        (cx + rx, cy - ky),
        (cx + rx, cy),
    ));
    path.push_str("h\n");
    path
}

/// The form XObject: `/BBox` is the annotation's Rect so §12.5.5's fit is
/// the identity, and a `/CA` below 1 becomes an `/ExtGState` the content
/// selects first.
fn form(bbox: [f32; 4], opacity: Option<f32>, content: String) -> Stream {
    let mut dict = Dict::new();
    dict.insert(name("Type"), Object::Name(name("XObject")));
    dict.insert(name("Subtype"), Object::Name(name("Form")));
    dict.insert(
        name("BBox"),
        Object::Array(bbox.iter().map(|v| Object::Real(f64::from(*v))).collect()),
    );
    if let Some(ca) = opacity {
        let mut state = Dict::new();
        state.insert(name("CA"), Object::Real(f64::from(ca)));
        state.insert(name("ca"), Object::Real(f64::from(ca)));
        let mut states = Dict::new();
        states.insert(name(OPACITY_STATE), Object::Dict(state));
        let mut resources = Dict::new();
        resources.insert(name("ExtGState"), Object::Dict(states));
        dict.insert(name("Resources"), Object::Dict(resources));
    }
    Stream {
        dict,
        data: content.into_bytes(),
    }
}

/// The rectangle with its corners ordered: `[x0 y0 x1 y1]`, x0 <= x1 and
/// y0 <= y1.
fn normalized(rect: &[f32]) -> [f32; 4] {
    [
        rect[0].min(rect[2]),
        rect[1].min(rect[3]),
        rect[0].max(rect[2]),
        rect[1].max(rect[3]),
    ]
}

fn name(s: &str) -> Name {
    Name(s.to_string())
}

fn num(v: f32) -> String {
    format!("{v}")
}

fn nums(values: &[f32]) -> String {
    values.iter().map(|v| num(*v)).collect::<Vec<_>>().join(" ")
}
