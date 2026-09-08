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

/// A line ending's size relative to the line width, and its floor in
/// default user space units.
const ENDING_SCALE: f32 = 6.0;
const ENDING_MIN: f32 = 4.0;

/// A form XObject painting the annotation from its own entries, its
/// `/BBox` in default user space: `None` for a subtype whose appearance is
/// not synthesized, or for one whose entries describe nothing to paint (no
/// colour, or a zero-width border and no interior).
///
/// Covers ISO 32000-1 §12.5.6.7, §12.5.6.8 and §12.5.6.9.
pub(crate) async fn synthesized<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Option<Stream> {
    let subtype = annot.get_name("Subtype")?.0.clone();
    let paint = Paint::read(src, annot).await;
    let (bbox, content) = match subtype.as_str() {
        "Square" | "Circle" => {
            let rect = normalized(&floats_from(src, annot.get("Rect"), 4).await?);
            let content = paint.shape(inner(rect, &paint.differences), subtype == "Circle")?;
            (rect, content)
        }
        "Line" => {
            let l = floats_from(src, annot.get("L"), 4).await?;
            let endings = Ending::pair(src, annot).await;
            let leader = Leader::read(src, annot).await;
            paint.line([l[0], l[1]], [l[2], l[3]], endings, leader)?
        }
        "Polygon" => {
            let vertices = number_list(src, annot.get("Vertices")?).await?;
            paint.polyline(&vertices, true, [Ending::None, Ending::None])?
        }
        "PolyLine" => {
            let vertices = number_list(src, annot.get("Vertices")?).await?;
            let endings = Ending::pair(src, annot).await;
            paint.polyline(&vertices, false, endings)?
        }
        _ => return None,
    };
    Some(form(bbox, paint.opacity, content))
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

    /// The stroke colour operator, when there is a colour and a width.
    fn stroking(&self) -> Option<&str> {
        self.stroke.as_deref().filter(|_| self.border.width > 0.0)
    }

    /// `q`, the opacity state, the colours, the width and the dash pattern:
    /// what every synthesized content stream opens with.
    fn preamble(&self) -> String {
        let mut content = String::from("q\n");
        if self.opacity.is_some() {
            content.push_str(&format!("/{OPACITY_STATE} gs\n"));
        }
        if let Some(op) = self.stroking() {
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
        content
    }

    /// The content that fills `rect` with the interior colour and strokes
    /// its outline, a rectangle or the inscribed ellipse, with the border
    /// drawn completely inside it (§12.5.4); `None` when nothing paints.
    ///
    /// Covers ISO 32000-1 §12.5.6.8.
    fn shape(&self, rect: [f32; 4], ellipse: bool) -> Option<String> {
        let stroke = self.stroking();
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
        let mut content = self.preamble();
        if ellipse {
            content.push_str(&ellipse_path(
                [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
                (x1 - x0) / 2.0,
                (y1 - y0) / 2.0,
            ));
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

    /// The content of a line annotation and the box it covers: the line
    /// from `p1` to `p2`, moved sideways by the leader length when there
    /// are leader lines, the leader lines and their extensions, and a line
    /// ending at each end, the closed ones filled with the interior colour.
    /// `None` without a stroke colour and width, or for a zero-length line.
    ///
    /// Covers ISO 32000-1 §12.5.6.7.
    fn line(
        &self,
        p1: [f32; 2],
        p2: [f32; 2],
        endings: [Ending; 2],
        leader: Leader,
    ) -> Option<([f32; 4], String)> {
        self.stroking()?;
        let (dx, dy) = (p2[0] - p1[0], p2[1] - p1[1]);
        let length = (dx * dx + dy * dy).sqrt();
        if !length.is_finite() || length <= 0.0 {
            return None;
        }
        let along = [dx / length, dy / length];
        // Clockwise from the direction of travel: where a positive /LL goes.
        let right = [along[1], -along[0]];
        let shifted = |p: [f32; 2], by: f32| [p[0] + right[0] * by, p[1] + right[1] * by];
        let a = shifted(p1, leader.length);
        let b = shifted(p2, leader.length);
        let mut points = vec![p1, p2, a, b];
        let mut content = self.preamble();
        content.push_str(&format!(
            "{} {} m {} {} l S\n",
            num(a[0]),
            num(a[1]),
            num(b[0]),
            num(b[1])
        ));
        if leader.length != 0.0 {
            let sign = leader.length.signum();
            for p in [p1, p2] {
                let from = shifted(p, sign * leader.offset);
                let to = shifted(p, leader.length + sign * leader.extension);
                content.push_str(&format!(
                    "{} {} m {} {} l S\n",
                    num(from[0]),
                    num(from[1]),
                    num(to[0]),
                    num(to[1])
                ));
                points.push(from);
                points.push(to);
            }
        }
        let size = (self.border.width * ENDING_SCALE)
            .max(ENDING_MIN)
            .min(length);
        for (point, outward, ending) in [
            (a, [-along[0], -along[1]], endings[0]),
            (b, along, endings[1]),
        ] {
            content.push_str(&ending.content(
                point,
                outward,
                right,
                size,
                self.fill.is_some(),
                &mut points,
            ));
        }
        content.push_str("Q\n");
        let margin = self.border.width + size;
        Some((bounds(&points, margin), content))
    }
}

impl Paint {
    /// The content of a polygon or polyline through `coords`, alternating
    /// x and y, and the box it covers: the vertices joined by straight
    /// lines, closed back to the first and filled with the interior colour
    /// for a polygon, left open with a line ending at each end for a
    /// polyline. `None` with fewer than two vertices or nothing to paint.
    ///
    /// Covers ISO 32000-1 §12.5.6.9.
    fn polyline(
        &self,
        coords: &[f32],
        closed: bool,
        endings: [Ending; 2],
    ) -> Option<([f32; 4], String)> {
        let vertices: Vec<[f32; 2]> = coords.as_chunks::<2>().0.to_vec();
        if vertices.len() < 2 {
            return None;
        }
        let stroke = self.stroking();
        let operator = match (closed, self.fill.as_deref(), stroke) {
            (true, Some(_), Some(_)) => "b",
            (true, Some(_), None) => "f",
            (true, None, Some(_)) => "s",
            (false, _, Some(_)) => "S",
            (_, None, None) | (false, Some(_), None) => return None,
        };
        let mut content = self.preamble();
        for (i, v) in vertices.iter().enumerate() {
            content.push_str(&format!(
                "{} {} {}\n",
                num(v[0]),
                num(v[1]),
                if i == 0 { "m" } else { "l" }
            ));
        }
        content.push_str(operator);
        content.push('\n');
        let mut points = vertices.clone();
        let mut size = 0.0;
        if !closed {
            let first = vertices[0];
            let last = vertices[vertices.len() - 1];
            let ends = [
                (first, vertices[1], endings[0]),
                (last, vertices[vertices.len() - 2], endings[1]),
            ];
            for (point, neighbour, ending) in ends {
                let (dx, dy) = (point[0] - neighbour[0], point[1] - neighbour[1]);
                let length = (dx * dx + dy * dy).sqrt();
                if !length.is_finite() || length <= 0.0 {
                    continue;
                }
                let outward = [dx / length, dy / length];
                let side = [outward[1], -outward[0]];
                size = (self.border.width * ENDING_SCALE)
                    .max(ENDING_MIN)
                    .min(length);
                content.push_str(&ending.content(
                    point,
                    outward,
                    side,
                    size,
                    self.fill.is_some(),
                    &mut points,
                ));
            }
        }
        content.push_str("Q\n");
        Some((bounds(&points, self.border.width + size), content))
    }
}

/// A line ending style of Table 176.
#[derive(Clone, Copy, PartialEq)]
enum Ending {
    None,
    Square,
    Circle,
    Diamond,
    OpenArrow,
    ClosedArrow,
    Butt,
    ROpenArrow,
    RClosedArrow,
    Slash,
}

impl Ending {
    /// The `/LE` pair, `[/None /None]` when absent or unreadable.
    async fn pair<S: AsyncObjectSource>(src: &S, annot: &Dict) -> [Ending; 2] {
        let Some(le) = annot.get("LE") else {
            return [Ending::None, Ending::None];
        };
        let Ok(Object::Array(items)) = src.resolve(le).await else {
            return [Ending::None, Ending::None];
        };
        let at = |i: usize| {
            items
                .get(i)
                .and_then(Object::as_name)
                .map_or(Ending::None, |n| Ending::named(&n.0))
        };
        [at(0), at(1)]
    }

    fn named(name: &str) -> Ending {
        match name {
            "Square" => Ending::Square,
            "Circle" => Ending::Circle,
            "Diamond" => Ending::Diamond,
            "OpenArrow" => Ending::OpenArrow,
            "ClosedArrow" => Ending::ClosedArrow,
            "Butt" => Ending::Butt,
            "ROpenArrow" => Ending::ROpenArrow,
            "RClosedArrow" => Ending::RClosedArrow,
            "Slash" => Ending::Slash,
            _ => Ending::None,
        }
    }

    /// The ending drawn at `point`, `outward` being the unit vector that
    /// leaves the line there and `side` the unit vector across it; `size`
    /// is the ending's extent. Closed shapes fill when `filled`. Every
    /// point drawn is added to `points` for the bounding box.
    ///
    /// Covers ISO 32000-1 §12.5.6.7.
    fn content(
        self,
        point: [f32; 2],
        outward: [f32; 2],
        side: [f32; 2],
        size: f32,
        filled: bool,
        points: &mut Vec<[f32; 2]>,
    ) -> String {
        let at = |o: f32, s: f32| {
            [
                point[0] + outward[0] * o + side[0] * s,
                point[1] + outward[1] * o + side[1] * s,
            ]
        };
        let half = size / 2.0;
        let closed = if filled { "b" } else { "s" };
        let (path, operator): (Vec<[f32; 2]>, &str) = match self {
            Ending::None => return String::new(),
            Ending::Square => (
                vec![
                    at(half, half),
                    at(half, -half),
                    at(-half, -half),
                    at(-half, half),
                ],
                closed,
            ),
            Ending::Diamond => (
                vec![at(half, 0.0), at(0.0, -half), at(-half, 0.0), at(0.0, half)],
                closed,
            ),
            Ending::OpenArrow => (vec![at(-size, half), point, at(-size, -half)], "S"),
            Ending::ClosedArrow => (vec![at(-size, half), point, at(-size, -half)], closed),
            Ending::ROpenArrow => (vec![at(size, half), point, at(size, -half)], "S"),
            Ending::RClosedArrow => (vec![at(size, half), point, at(size, -half)], closed),
            Ending::Butt => (vec![at(0.0, half), at(0.0, -half)], "S"),
            Ending::Slash => {
                // 30 degrees clockwise from the perpendicular.
                let (sin, cos) = (0.5, 0.866_025_4);
                (
                    vec![at(sin * half, cos * half), at(-sin * half, -cos * half)],
                    "S",
                )
            }
            Ending::Circle => {
                points.push(at(half, half));
                points.push(at(-half, -half));
                let mut content = ellipse_path(point, half, half);
                content.push_str(if filled { "B\n" } else { "S\n" });
                return content;
            }
        };
        points.extend_from_slice(&path);
        let mut content = String::new();
        for (i, p) in path.iter().enumerate() {
            content.push_str(&format!(
                "{} {} {}\n",
                num(p[0]),
                num(p[1]),
                if i == 0 { "m" } else { "l" }
            ));
        }
        content.push_str(operator);
        content.push('\n');
        content
    }
}

/// The leader lines of a line annotation (Table 175): `/LL` is their
/// length, positive on the clockwise side of the line; `/LLE` extends them
/// past the line and `/LLO` leaves a gap at the annotation's endpoints.
#[derive(Clone, Copy)]
struct Leader {
    length: f32,
    extension: f32,
    offset: f32,
}

impl Leader {
    async fn read<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Leader {
        Leader {
            length: dict_f32(src, annot, "LL").await.unwrap_or(0.0),
            extension: dict_f32(src, annot, "LLE").await.unwrap_or(0.0).max(0.0),
            offset: dict_f32(src, annot, "LLO").await.unwrap_or(0.0).max(0.0),
        }
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

/// Every number of a (possibly indirect) array, resolving indirect
/// entries; `None` when the object is not an array or an entry is not a
/// finite number.
async fn number_list<S: AsyncObjectSource>(src: &S, obj: &Object) -> Option<Vec<f32>> {
    let Ok(Object::Array(items)) = src.resolve(obj).await else {
        return None;
    };
    let mut numbers = Vec::with_capacity(items.len());
    for item in &items {
        numbers.push(num_f32(src, item).await?);
    }
    Some(numbers)
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

/// The four Bezier arcs of the ellipse with the given centre and radii,
/// closed.
fn ellipse_path(centre: [f32; 2], rx: f32, ry: f32) -> String {
    let [cx, cy] = centre;
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

/// The box around `points`, grown by `margin` on every side.
fn bounds(points: &[[f32; 2]], margin: f32) -> [f32; 4] {
    let mut box_ = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for p in points {
        box_[0] = box_[0].min(p[0]);
        box_[1] = box_[1].min(p[1]);
        box_[2] = box_[2].max(p[0]);
        box_[3] = box_[3].max(p[1]);
    }
    [
        box_[0] - margin,
        box_[1] - margin,
        box_[2] + margin,
        box_[3] + margin,
    ]
}

/// The form XObject: `/BBox` is the box the content covers in default user
/// space, so §12.5.5's fit is the identity, and a `/CA` below 1 becomes an
/// `/ExtGState` the content selects first.
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
