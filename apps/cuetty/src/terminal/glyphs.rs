//! Box drawing, block elements, and Powerline separators drawn as shapes.
//!
//! A font draws these to fit its own metrics, which never quite match a cell whose
//! height comes from the configured line height, so rules show seams between rows.
//! Drawn from the cell's device pixels instead they meet exactly. Everything here is
//! plain geometry in device pixels, so it is tested without a window.

use super::sizing::CellSize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeviceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The stippled blocks, drawn as the foreground at partial strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shade {
    Light,
    Medium,
    Dark,
}

impl Shade {
    pub(super) const fn opacity(self) -> f32 {
        match self {
            Self::Light => 0.25,
            Self::Medium => 0.5,
            Self::Dark => 0.75,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum GlyphShape {
    /// Aligned to device pixels, so it needs no antialiasing and leaves no seam.
    Rect(DeviceRect),
    /// The whole cell.
    Shade(Shade),
    /// A filled outline of device pixel coordinates, for edges that are not straight.
    Polygon(Vec<(f32, f32)>),
}

/// The shapes for a character this module draws, or `None` to leave it to the font.
pub(super) fn drawn_glyph(character: char, cell: CellSize) -> Option<Vec<GlyphShape>> {
    let cell = CellSize {
        width: cell.width.max(1),
        height: cell.height.max(1),
    };
    match u32::from(character) {
        0x2500..=0x257F => Some(box_drawing(character, cell)),
        0x2580..=0x259F => Some(block_element(character, cell)),
        0xE0B0..=0xE0B3 => Some(powerline(character, cell)),
        _ => None,
    }
}

/// Whether [`drawn_glyph`] draws this character.
pub(super) fn is_drawn(character: char) -> bool {
    matches!(
        u32::from(character),
        0x2500..=0x259F | 0xE0B0..=0xE0B3
    )
}

/// Whether a character is a piece of a picture rather than text. Image previews and
/// Powerline segments are built from these, however they are painted, and only line up
/// when a cell's foreground is exactly the colour it was given.
pub(super) fn is_graphic(character: char) -> bool {
    is_drawn(character)
        || matches!(
            u32::from(character),
            // Braille, used for its dots.
            0x2800..=0x28FF
            // Powerline Extra's rounded, slanted, and decorative separators.
            | 0xE0B4..=0xE0D7
            // Symbols for Legacy Computing and its supplement: sextants, octants, wedges.
            | 0x1CC00..=0x1CEBF
            | 0x1FB00..=0x1FBFF
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Weight {
    None,
    Light,
    Heavy,
    Double,
}

/// Which way a line leaves the middle of a cell, at what weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arms {
    up: Weight,
    right: Weight,
    down: Weight,
    left: Weight,
}

/// Arms for U+2500 to U+257F in order, as up, right, down, left: `-` none, `L` light,
/// `H` heavy, `D` double. `?` marks the dashes, arcs, and diagonals that are not arms.
const BOX_ARMS: [&[u8; 4]; 128] = [
    b"-L-L", b"-H-H", b"L-L-", b"H-H-", b"????", b"????", b"????", b"????", //
    b"????", b"????", b"????", b"????", b"-LL-", b"-HL-", b"-LH-", b"-HH-", //
    b"--LL", b"--LH", b"--HL", b"--HH", b"LL--", b"LH--", b"HL--", b"HH--", //
    b"L--L", b"L--H", b"H--L", b"H--H", b"LLL-", b"LHL-", b"HLL-", b"LLH-", //
    b"HLH-", b"HHL-", b"LHH-", b"HHH-", b"L-LL", b"L-LH", b"H-LL", b"L-HL", //
    b"H-HL", b"H-LH", b"L-HH", b"H-HH", b"-LLL", b"-LLH", b"-HLL", b"-HLH", //
    b"-LHL", b"-LHH", b"-HHL", b"-HHH", b"LL-L", b"LL-H", b"LH-L", b"LH-H", //
    b"HL-L", b"HL-H", b"HH-L", b"HH-H", b"LLLL", b"LLLH", b"LHLL", b"LHLH", //
    b"HLLL", b"LLHL", b"HLHL", b"HLLH", b"HHLL", b"LLHH", b"LHHL", b"HHLH", //
    b"LHHH", b"HLHH", b"HHHL", b"HHHH", b"????", b"????", b"????", b"????", //
    b"-D-D", b"D-D-", b"-DL-", b"-LD-", b"-DD-", b"--LD", b"--DL", b"--DD", //
    b"LD--", b"DL--", b"DD--", b"L--D", b"D--L", b"D--D", b"LDL-", b"DLD-", //
    b"DDD-", b"L-LD", b"D-DL", b"D-DD", b"-DLD", b"-LDL", b"-DDD", b"LD-D", //
    b"DL-L", b"DD-D", b"LDLD", b"DLDL", b"DDDD", b"????", b"????", b"????", //
    b"????", b"????", b"????", b"????", b"---L", b"L---", b"-L--", b"--L-", //
    b"---H", b"H---", b"-H--", b"--H-", b"-H-L", b"L-H-", b"-L-H", b"H-L-", //
];

fn weight_from(code: u8) -> Weight {
    match code {
        b'L' => Weight::Light,
        b'H' => Weight::Heavy,
        b'D' => Weight::Double,
        _ => Weight::None,
    }
}

fn box_arms(character: char) -> Option<Arms> {
    let offset = u32::from(character).checked_sub(0x2500)?;
    let [up, right, down, left] = BOX_ARMS
        .get(usize::try_from(offset).ok()?)?
        .map(weight_from);
    (up, right, down, left)
        .ne(&(Weight::None, Weight::None, Weight::None, Weight::None))
        .then_some(Arms {
            up,
            right,
            down,
            left,
        })
}

/// A stroke in a cell: light is about a sixteenth of a line's height, so it keeps its
/// weight relative to the text at any font size or scale.
#[derive(Debug, Clone, Copy)]
struct Pen {
    light: u32,
}

impl Pen {
    fn for_cell(cell: CellSize) -> Self {
        Self {
            light: ((cell.height + 9) / 18).max(1),
        }
    }

    /// How wide a line of this weight is across, gap included.
    fn width(self, weight: Weight) -> u32 {
        self.light
            * match weight {
                Weight::None => 0,
                Weight::Light => 1,
                Weight::Heavy => 2,
                Weight::Double => 3,
            }
    }
}

/// A run of device pixels along one axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: u32,
    end: u32,
}

/// `width` pixels centred in `extent`. Every line of one weight lands on the same
/// pixels, which is what makes neighbouring cells meet.
fn centred(extent: u32, width: u32) -> Span {
    let start = extent.saturating_sub(width) / 2;
    Span {
        start,
        end: (start + width).min(extent),
    }
}

/// Which end of an axis an arm runs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Start,
    End,
}

impl Side {
    /// The edge of a band that an arm from this side reaches last.
    fn far_edge(self, band: Span) -> u32 {
        match self {
            Self::Start => band.end,
            Self::End => band.start,
        }
    }

    /// The arm's whole run, from the cell's edge to a stopping point.
    fn run(self, extent: u32, stop: u32) -> Span {
        match self {
            Self::Start => Span {
                start: 0,
                end: stop.min(extent),
            },
            Self::End => Span {
                start: stop.min(extent),
                end: extent,
            },
        }
    }
}

/// The two strokes of a double line.
fn double_strokes(extent: u32, pen: Pen) -> (Span, Span) {
    let band = centred(extent, pen.width(Weight::Double));
    (
        Span {
            start: band.start,
            end: (band.start + pen.light).min(band.end),
        },
        Span {
            start: band.end.saturating_sub(pen.light).max(band.start),
            end: band.end,
        },
    )
}

/// How long an axis is, and the pen drawing along it.
#[derive(Debug, Clone, Copy)]
struct AxisPen {
    extent: u32,
    pen: Pen,
}

/// One axis of a junction, so that horizontal and vertical arms share one set of rules.
#[derive(Debug, Clone, Copy)]
struct AxisArms {
    start: Weight,
    end: Weight,
    /// The crossing arm on the lower-coordinate side.
    before: Weight,
    /// The crossing arm on the higher-coordinate side.
    after: Weight,
}

/// A light or heavy arm, and the arm facing it across the junction.
#[derive(Debug, Clone, Copy)]
struct SingleArm {
    side: Side,
    opposite: Weight,
}

/// Where a light or heavy arm stops. It reaches across the line it meets, unless that
/// is a double line it does not cross, which it stops at the near stroke of.
fn single_stop(arm: SingleArm, arms: AxisArms, axis: AxisPen) -> u32 {
    let SingleArm { side, opposite } = arm;
    let AxisPen { extent, pen } = axis;
    let own = match side {
        Side::Start => arms.start,
        Side::End => arms.end,
    };
    if arms.before == Weight::Double || arms.after == Weight::Double {
        if arms.before == arms.after && opposite == Weight::None {
            inner_edge(side, axis)
        } else {
            side.far_edge(centred(extent, pen.width(Weight::Double)))
        }
    } else {
        let met = arms.before.max(arms.after);
        let width = pen.width(if met == Weight::None { own } else { met });
        side.far_edge(centred(extent, width))
    }
}

/// The edge of a double line's nearer stroke, on the side away from this arm's own edge.
fn inner_edge(side: Side, axis: AxisPen) -> u32 {
    let (first, second) = double_strokes(axis.extent, axis.pen);
    match side {
        Side::Start => first.end,
        Side::End => second.start,
    }
}

/// One stroke of a double arm, and the crossing arms on either side of that stroke.
#[derive(Debug, Clone, Copy)]
struct DoubleStroke {
    side: Side,
    /// The crossing arm on this stroke's side.
    beside: Weight,
    /// The crossing arm on this stroke's far side.
    other: Weight,
}

/// Where one stroke of a double arm stops: a double arm beside it turns the stroke into
/// an inner corner, and a stroke with nothing beside it runs around the outside.
fn double_stop(stroke: DoubleStroke, axis: AxisPen) -> u32 {
    let DoubleStroke {
        side,
        beside,
        other,
    } = stroke;
    match (beside, other) {
        (Weight::Double, _) => inner_edge(side, axis),
        (Weight::None, Weight::None) => axis.extent / 2,
        (Weight::None, met) | (met, _) => side.far_edge(centred(axis.extent, axis.pen.width(met))),
    }
}

/// The strokes along one axis, as a run along it and a band across it.
fn axis_strokes(along: AxisPen, cross: u32, arms: AxisArms) -> Vec<(Span, Span)> {
    let AxisPen { extent: main, pen } = along;
    [
        (Side::Start, arms.start, arms.end),
        (Side::End, arms.end, arms.start),
    ]
    .into_iter()
    .flat_map(|(side, weight, opposite)| match weight {
        Weight::None => Vec::new(),
        Weight::Light | Weight::Heavy => vec![(
            side.run(main, single_stop(SingleArm { side, opposite }, arms, along)),
            centred(cross, pen.width(weight)),
        )],
        Weight::Double => {
            let (first, second) = double_strokes(cross, pen);
            vec![
                (
                    side.run(
                        main,
                        double_stop(
                            DoubleStroke {
                                side,
                                beside: arms.before,
                                other: arms.after,
                            },
                            along,
                        ),
                    ),
                    first,
                ),
                (
                    side.run(
                        main,
                        double_stop(
                            DoubleStroke {
                                side,
                                beside: arms.after,
                                other: arms.before,
                            },
                            along,
                        ),
                    ),
                    second,
                ),
            ]
        }
    })
    .collect()
}

fn rect_from(x: Span, y: Span) -> Option<GlyphShape> {
    (x.end > x.start && y.end > y.start).then_some(GlyphShape::Rect(DeviceRect {
        x: x.start,
        y: y.start,
        width: x.end - x.start,
        height: y.end - y.start,
    }))
}

fn junction(arms: Arms, cell: CellSize) -> Vec<GlyphShape> {
    let pen = Pen::for_cell(cell);
    let horizontal = axis_strokes(
        AxisPen {
            extent: cell.width,
            pen,
        },
        cell.height,
        AxisArms {
            start: arms.left,
            end: arms.right,
            before: arms.up,
            after: arms.down,
        },
    );
    let vertical = axis_strokes(
        AxisPen {
            extent: cell.height,
            pen,
        },
        cell.width,
        AxisArms {
            start: arms.up,
            end: arms.down,
            before: arms.left,
            after: arms.right,
        },
    );
    horizontal
        .into_iter()
        .chain(vertical.into_iter().map(|(y, x)| (x, y)))
        .filter_map(|(x, y)| rect_from(x, y))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// A line broken into dashes.
#[derive(Debug, Clone, Copy)]
struct DashRun {
    axis: Axis,
    weight: Weight,
    count: u32,
}

impl DashRun {
    const fn horizontal(weight: Weight, count: u32) -> Self {
        Self {
            axis: Axis::Horizontal,
            weight,
            count,
        }
    }

    const fn vertical(weight: Weight, count: u32) -> Self {
        Self {
            axis: Axis::Vertical,
            weight,
            count,
        }
    }
}

/// Each dash is centred in an equal slot, so the pattern continues across cells.
fn dashes(run: DashRun, cell: CellSize) -> Vec<GlyphShape> {
    let DashRun {
        axis,
        weight,
        count,
    } = run;
    let pen = Pen::for_cell(cell);
    let (main, cross) = match axis {
        Axis::Horizontal => (cell.width, cell.height),
        Axis::Vertical => (cell.height, cell.width),
    };
    let band = centred(cross, pen.width(weight));
    (0..count)
        .filter_map(|index| {
            let slot_start = main * index / count;
            let slot_end = main * (index + 1) / count;
            let gap = ((slot_end - slot_start) / 4).max(1);
            let dash = Span {
                start: slot_start + gap / 2,
                end: slot_end.saturating_sub(gap - gap / 2),
            };
            match axis {
                Axis::Horizontal => rect_from(dash, band),
                Axis::Vertical => rect_from(band, dash),
            }
        })
        .collect()
}

/// Which edges of the cell an arc's two ends run to: `Side::End` is right or bottom.
#[derive(Debug, Clone, Copy)]
struct ArcCorner {
    horizontal: Side,
    vertical: Side,
}

/// How many straight segments stand in for a quarter circle.
const ARC_SEGMENTS: u32 = 12;

/// A rounded corner on the same pixels as the straight lines it joins.
fn arc(corner: ArcCorner, cell: CellSize) -> Vec<GlyphShape> {
    let pen = Pen::for_cell(cell);
    let stroke = pen.width(Weight::Light) as f32;
    let x_band = centred(cell.width, pen.width(Weight::Light));
    let y_band = centred(cell.height, pen.width(Weight::Light));
    let (width, height) = (cell.width as f32, cell.height as f32);
    let (x_dir, outer_x, end_x) = match corner.horizontal {
        Side::End => (1.0, x_band.start as f32, width),
        Side::Start => (-1.0, x_band.end as f32, 0.0),
    };
    let (y_dir, outer_y, end_y) = match corner.vertical {
        Side::End => (1.0, y_band.start as f32, height),
        Side::Start => (-1.0, y_band.end as f32, 0.0),
    };
    let radius = (end_x - outer_x)
        .abs()
        .min((end_y - outer_y).abs())
        .max(stroke);
    let (center_x, center_y) = (outer_x + x_dir * radius, outer_y + y_dir * radius);
    let quarter = |radius: f32, step: u32| {
        let angle = std::f32::consts::FRAC_PI_2 * step as f32 / ARC_SEGMENTS as f32;
        // Clamped because a cosine at a quarter turn is not quite zero.
        (
            (center_x - x_dir * radius * angle.cos()).clamp(0.0, width),
            (center_y - y_dir * radius * angle.sin()).clamp(0.0, height),
        )
    };
    let inner = (radius - stroke).max(0.0);
    let mut outline = vec![(outer_x, end_y)];
    outline.extend((0..=ARC_SEGMENTS).map(|step| quarter(radius, step)));
    outline.push((end_x, outer_y));
    outline.push((end_x, outer_y + y_dir * stroke));
    outline.extend((0..=ARC_SEGMENTS).rev().map(|step| quarter(inner, step)));
    outline.push((outer_x + x_dir * stroke, end_y));
    vec![GlyphShape::Polygon(outline)]
}

/// Which way a diagonal leans, read left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slant {
    Rising,
    Falling,
}

/// A line centred on the cell's opposite corners, as heavy as a light rule. It is cut off
/// at the cell's edges, because past them it would paint over the next column, and the
/// neighbouring diagonal supplies the rest of the stroke where two of them meet.
fn diagonal(slant: Slant, cell: CellSize) -> GlyphShape {
    let (width, height) = (cell.width as f32, cell.height as f32);
    let stroke = Pen::for_cell(cell).width(Weight::Light) as f32;
    // How far the stroke reaches from its centre line along a row, and along a column.
    let across = (stroke * width.hypot(height) / height / 2.0).min(width);
    let along = across * height / width;
    let rising = [
        (width - across, 0.0),
        (width, 0.0),
        (width, along),
        (across, height),
        (0.0, height),
        (0.0, height - along),
    ];
    GlyphShape::Polygon(
        rising
            .into_iter()
            .map(|(x, y)| match slant {
                Slant::Rising => (x, y),
                Slant::Falling => (width - x, y),
            })
            .collect(),
    )
}

fn box_drawing(character: char, cell: CellSize) -> Vec<GlyphShape> {
    if let Some(arms) = box_arms(character) {
        return junction(arms, cell);
    }
    let corner = |horizontal, vertical| ArcCorner {
        horizontal,
        vertical,
    };
    match character {
        '\u{2504}' => dashes(DashRun::horizontal(Weight::Light, 3), cell),
        '\u{2505}' => dashes(DashRun::horizontal(Weight::Heavy, 3), cell),
        '\u{2506}' => dashes(DashRun::vertical(Weight::Light, 3), cell),
        '\u{2507}' => dashes(DashRun::vertical(Weight::Heavy, 3), cell),
        '\u{2508}' => dashes(DashRun::horizontal(Weight::Light, 4), cell),
        '\u{2509}' => dashes(DashRun::horizontal(Weight::Heavy, 4), cell),
        '\u{250A}' => dashes(DashRun::vertical(Weight::Light, 4), cell),
        '\u{250B}' => dashes(DashRun::vertical(Weight::Heavy, 4), cell),
        '\u{254C}' => dashes(DashRun::horizontal(Weight::Light, 2), cell),
        '\u{254D}' => dashes(DashRun::horizontal(Weight::Heavy, 2), cell),
        '\u{254E}' => dashes(DashRun::vertical(Weight::Light, 2), cell),
        '\u{254F}' => dashes(DashRun::vertical(Weight::Heavy, 2), cell),
        '\u{256D}' => arc(corner(Side::End, Side::End), cell),
        '\u{256E}' => arc(corner(Side::Start, Side::End), cell),
        '\u{256F}' => arc(corner(Side::Start, Side::Start), cell),
        '\u{2570}' => arc(corner(Side::End, Side::Start), cell),
        '\u{2571}' => vec![diagonal(Slant::Rising, cell)],
        '\u{2572}' => vec![diagonal(Slant::Falling, cell)],
        '\u{2573}' => vec![
            diagonal(Slant::Rising, cell),
            diagonal(Slant::Falling, cell),
        ],
        _ => Vec::new(),
    }
}

/// `eighths` eighths of `length`, rounded once so that pieces cut at the same place
/// meet with neither a gap nor an overlap.
fn eighths_of(length: u32, eighths: u32) -> u32 {
    (length * eighths + 4) / 8
}

#[derive(Debug, Clone, Copy)]
enum Quadrant {
    UpperLeft,
    UpperRight,
    LowerLeft,
    LowerRight,
}

fn block_element(character: char, cell: CellSize) -> Vec<GlyphShape> {
    let (width, height) = (cell.width, cell.height);
    let whole_x = Span {
        start: 0,
        end: width,
    };
    let whole_y = Span {
        start: 0,
        end: height,
    };
    let top = |eighths| Span {
        start: 0,
        end: eighths_of(height, eighths).max(1),
    };
    let bottom = |eighths: u32| Span {
        start: eighths_of(height, 8 - eighths).min(height - 1),
        end: height,
    };
    let left = |eighths| Span {
        start: 0,
        end: eighths_of(width, eighths).max(1),
    };
    let right = |eighths: u32| Span {
        start: eighths_of(width, 8 - eighths).min(width - 1),
        end: width,
    };
    let quadrants = |corners: &[Quadrant]| {
        corners
            .iter()
            .filter_map(|corner| match corner {
                Quadrant::UpperLeft => rect_from(left(4), top(4)),
                Quadrant::UpperRight => rect_from(right(4), top(4)),
                Quadrant::LowerLeft => rect_from(left(4), bottom(4)),
                Quadrant::LowerRight => rect_from(right(4), bottom(4)),
            })
            .collect()
    };
    use Quadrant::{LowerLeft, LowerRight, UpperLeft, UpperRight};
    let code = u32::from(character);
    match code {
        0x2580 => rect_from(whole_x, top(4)).into_iter().collect(),
        0x2581..=0x2588 => rect_from(whole_x, bottom(code - 0x2580))
            .into_iter()
            .collect(),
        0x2589..=0x258F => rect_from(left(0x2590 - code), whole_y)
            .into_iter()
            .collect(),
        0x2590 => rect_from(right(4), whole_y).into_iter().collect(),
        0x2591 => vec![GlyphShape::Shade(Shade::Light)],
        0x2592 => vec![GlyphShape::Shade(Shade::Medium)],
        0x2593 => vec![GlyphShape::Shade(Shade::Dark)],
        0x2594 => rect_from(whole_x, top(1)).into_iter().collect(),
        0x2595 => rect_from(right(1), whole_y).into_iter().collect(),
        0x2596 => quadrants(&[LowerLeft]),
        0x2597 => quadrants(&[LowerRight]),
        0x2598 => quadrants(&[UpperLeft]),
        0x2599 => quadrants(&[UpperLeft, LowerLeft, LowerRight]),
        0x259A => quadrants(&[UpperLeft, LowerRight]),
        0x259B => quadrants(&[UpperLeft, UpperRight, LowerLeft]),
        0x259C => quadrants(&[UpperLeft, UpperRight, LowerRight]),
        0x259D => quadrants(&[UpperRight]),
        0x259E => quadrants(&[UpperRight, LowerLeft]),
        0x259F => quadrants(&[UpperRight, LowerLeft, LowerRight]),
        _ => Vec::new(),
    }
}

/// Separators run the full height and meet the cell's edges exactly, so they join
/// the coloured segments on either side.
fn powerline(character: char, cell: CellSize) -> Vec<GlyphShape> {
    let (width, height) = (cell.width as f32, cell.height as f32);
    let middle = height / 2.0;
    // The chevron's stroke, measured along x, to look as heavy as a light rule.
    let stroke = Pen::for_cell(cell).width(Weight::Light) as f32;
    let span = (stroke * width.hypot(middle) / middle).min(width);
    let outline = match character {
        '\u{E0B0}' => vec![(0.0, 0.0), (width, middle), (0.0, height)],
        '\u{E0B2}' => vec![(width, 0.0), (0.0, middle), (width, height)],
        '\u{E0B1}' => vec![
            (0.0, 0.0),
            (span, 0.0),
            (width, middle),
            (span, height),
            (0.0, height),
            (width - span, middle),
        ],
        '\u{E0B3}' => vec![
            (width, 0.0),
            (width - span, 0.0),
            (0.0, middle),
            (width - span, height),
            (width, height),
            (span, middle),
        ],
        _ => return Vec::new(),
    };
    vec![GlyphShape::Polygon(outline)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellSize = CellSize {
        width: 11,
        height: 11,
    };

    fn covered(character: char, cell: CellSize) -> Vec<Vec<bool>> {
        let mut grid = vec![vec![false; cell.width as usize]; cell.height as usize];
        for shape in drawn_glyph(character, cell).expect("drawn character") {
            let GlyphShape::Rect(rect) = shape else {
                panic!("{character} is not made of rectangles");
            };
            assert!(rect.x + rect.width <= cell.width, "{character} overflows");
            assert!(rect.y + rect.height <= cell.height, "{character} overflows");
            for row in &mut grid[rect.y as usize..(rect.y + rect.height) as usize] {
                row[rect.x as usize..(rect.x + rect.width) as usize].fill(true);
            }
        }
        grid
    }

    /// The glyph as text, for comparing whole shapes at a glance.
    fn picture(character: char) -> Vec<String> {
        covered(character, CELL)
            .iter()
            .map(|row| row.iter().map(|on| if *on { '#' } else { '.' }).collect())
            .collect()
    }

    /// The strokes where a glyph crosses each edge of its cell.
    fn edges(character: char, cell: CellSize) -> [Vec<bool>; 4] {
        let grid = covered(character, cell);
        [
            grid[0].clone(),
            grid.iter().map(|row| row[row.len() - 1]).collect(),
            grid[grid.len() - 1].clone(),
            grid.iter().map(|row| row[0]).collect(),
        ]
    }

    /// The straight line of each weight, along each axis.
    fn line(axis: Axis, weight: Weight) -> char {
        match (axis, weight) {
            (Axis::Horizontal, Weight::Light) => '\u{2500}',
            (Axis::Horizontal, Weight::Heavy) => '\u{2501}',
            (Axis::Horizontal, Weight::Double) => '\u{2550}',
            (Axis::Vertical, Weight::Light) => '\u{2502}',
            (Axis::Vertical, Weight::Heavy) => '\u{2503}',
            (Axis::Vertical, Weight::Double) => '\u{2551}',
            (_, Weight::None) => unreachable!("no line without a weight"),
        }
    }

    fn all_arm_characters() -> impl Iterator<Item = (char, Arms)> {
        (0x2500..=0x257F_u32).filter_map(|code| {
            let character = char::from_u32(code)?;
            box_arms(character).map(|arms| (character, arms))
        })
    }

    #[test]
    fn every_arm_meets_its_neighbouring_cell_on_the_pixels_a_straight_line_uses() {
        for cell in [
            CELL,
            CellSize {
                width: 10,
                height: 19,
            },
            CellSize {
                width: 19,
                height: 38,
            },
            CellSize {
                width: 14,
                height: 29,
            },
        ] {
            for (character, arms) in all_arm_characters() {
                let seen = edges(character, cell);
                let expected = [
                    (arms.up, Axis::Vertical),
                    (arms.right, Axis::Horizontal),
                    (arms.down, Axis::Vertical),
                    (arms.left, Axis::Horizontal),
                ];
                for (edge, (weight, axis)) in expected.into_iter().enumerate() {
                    let code = u32::from(character);
                    if weight == Weight::None {
                        assert!(
                            seen[edge].iter().all(|on| !on),
                            "U+{code:04X} touches edge {edge}, which has no arm, at {cell:?}"
                        );
                    } else {
                        assert_eq!(
                            seen[edge],
                            edges(line(axis, weight), cell)[edge],
                            "U+{code:04X} edge {edge} at {cell:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_double_corner_nests_its_strokes() {
        assert_eq!(
            picture('\u{2554}'),
            [
                "...........",
                "...........",
                "...........",
                "...........",
                "....#######",
                "....#......",
                "....#.#####",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
            ]
        );
    }

    #[test]
    fn a_double_cross_keeps_all_four_corners_open() {
        assert_eq!(
            picture('\u{256C}'),
            [
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "#####.#####",
                "...........",
                "#####.#####",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
            ]
        );
    }

    #[test]
    fn a_double_tee_leaves_the_far_stroke_unbroken() {
        assert_eq!(
            picture('\u{2566}'),
            [
                "...........",
                "...........",
                "...........",
                "...........",
                "###########",
                "...........",
                "#####.#####",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
            ]
        );
    }

    #[test]
    fn a_single_line_stops_at_the_double_line_it_meets() {
        assert_eq!(
            picture('\u{255F}'),
            [
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#####",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
                "....#.#....",
            ]
        );
    }

    #[test]
    fn a_single_line_crosses_a_double_line_it_continues_past() {
        let picture = picture('\u{256B}');
        assert_eq!(picture[5], "###########");
        assert_eq!(picture[0], "....#.#....");
    }

    #[test]
    fn a_mixed_corner_closes_without_a_gap() {
        assert_eq!(
            picture('\u{2552}'),
            [
                "...........",
                "...........",
                "...........",
                "...........",
                ".....######",
                ".....#.....",
                ".....######",
                ".....#.....",
                ".....#.....",
                ".....#.....",
                ".....#.....",
            ]
        );
    }

    #[test]
    fn a_heavy_arm_covers_the_light_line_it_joins() {
        assert_eq!(
            picture('\u{250D}'),
            [
                "...........",
                "...........",
                "...........",
                "...........",
                ".....######",
                ".....######",
                ".....#.....",
                ".....#.....",
                ".....#.....",
                ".....#.....",
                ".....#.....",
            ]
        );
    }

    #[test]
    fn strokes_keep_their_weight_as_the_scale_grows() {
        let thickness = |cell| {
            covered('\u{2502}', cell)[0]
                .iter()
                .filter(|on| **on)
                .count()
        };
        let one_x = thickness(CellSize {
            width: 10,
            height: 19,
        });
        let two_x = thickness(CellSize {
            width: 19,
            height: 38,
        });
        assert_eq!(one_x, 1);
        assert_eq!(two_x, 2);
    }

    #[test]
    fn dashes_leave_gaps_and_stay_on_the_lines_pixels() {
        let cell = CellSize {
            width: 12,
            height: 12,
        };
        for (dashed, count) in [('\u{2504}', 3), ('\u{2508}', 4), ('\u{254C}', 2)] {
            let shapes = drawn_glyph(dashed, cell).expect("drawn");
            assert_eq!(shapes.len(), count);
            let row = covered(dashed, cell)
                .into_iter()
                .position(|row| row.iter().any(|on| *on))
                .expect("a row is covered");
            assert_eq!(
                row,
                covered('\u{2500}', cell)
                    .into_iter()
                    .position(|row| row.iter().any(|on| *on))
                    .expect("a row is covered"),
            );
            assert!(covered(dashed, cell)[row].iter().any(|on| !on));
        }
    }

    fn polygon(character: char, cell: CellSize) -> Vec<(f32, f32)> {
        match drawn_glyph(character, cell).expect("drawn").as_slice() {
            [GlyphShape::Polygon(points)] => points.clone(),
            other => panic!("expected one polygon, got {other:?}"),
        }
    }

    #[test]
    fn a_rounded_corner_stays_in_its_cell_and_lands_on_the_straight_lines_pixels() {
        let cell = CellSize {
            width: 19,
            height: 38,
        };
        let vertical = covered('\u{2502}', cell)[0]
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(x, _)| x as f32)
            .collect::<Vec<_>>();
        let (x0, x1) = (vertical[0], vertical[vertical.len() - 1] + 1.0);
        let horizontal = covered('\u{2500}', cell)
            .iter()
            .enumerate()
            .filter(|(_, row)| row[0])
            .map(|(y, _)| y as f32)
            .collect::<Vec<_>>();
        let (y0, y1) = (horizontal[0], horizontal[horizontal.len() - 1] + 1.0);
        for (character, edge_y, edge_x) in [
            ('\u{256D}', 38.0, 19.0),
            ('\u{256E}', 38.0, 0.0),
            ('\u{256F}', 0.0, 0.0),
            ('\u{2570}', 0.0, 19.0),
        ] {
            let outline = polygon(character, cell);
            for &(x, y) in &outline {
                assert!((0.0..=19.0).contains(&x) && (0.0..=38.0).contains(&y));
            }
            for x in [x0, x1] {
                assert!(outline.contains(&(x, edge_y)), "{character} at x {x}");
            }
            for y in [y0, y1] {
                assert!(outline.contains(&(edge_x, y)), "{character} at y {y}");
            }
        }
    }

    #[test]
    fn a_powerline_arrow_points_the_way_it_is_named_and_fills_the_cell_height() {
        let cell = CellSize {
            width: 10,
            height: 20,
        };
        assert_eq!(
            polygon('\u{E0B0}', cell),
            [(0.0, 0.0), (10.0, 10.0), (0.0, 20.0)]
        );
        assert_eq!(
            polygon('\u{E0B2}', cell),
            [(10.0, 0.0), (0.0, 10.0), (10.0, 20.0)]
        );
    }

    #[test]
    fn a_powerline_chevron_is_the_arrow_with_its_middle_removed() {
        let cell = CellSize {
            width: 10,
            height: 20,
        };
        let right = polygon('\u{E0B1}', cell);
        assert!(right.contains(&(10.0, 10.0)), "the point reaches the edge");
        assert!(right.iter().any(|&(x, y)| y == 10.0 && x < 10.0 && x > 0.0));
        assert!(right.iter().all(|&(x, _)| (0.0..=10.0).contains(&x)));
        let left = polygon('\u{E0B3}', cell);
        assert_eq!(right.len(), left.len());
        for (&(x, y), &(mirror_x, mirror_y)) in right.iter().zip(&left) {
            assert!((x - (10.0 - mirror_x)).abs() < 0.001, "{x} and {mirror_x}");
            assert_eq!(y, mirror_y);
        }
    }

    #[test]
    fn a_diagonal_runs_corner_to_corner_within_the_cell() {
        let cell = CellSize {
            width: 10,
            height: 20,
        };
        let corners = [
            ('\u{2571}', [(0.0, 20.0), (10.0, 0.0)]),
            ('\u{2572}', [(0.0, 0.0), (10.0, 20.0)]),
        ];
        for (character, ends) in corners {
            let outline = polygon(character, cell);
            for end in ends {
                assert!(outline.contains(&end), "{character} misses {end:?}");
            }
            for (x, y) in outline {
                assert!((0.0..=10.0).contains(&x), "{character} leaves at x {x}");
                assert!((0.0..=20.0).contains(&y), "{character} leaves at y {y}");
            }
        }
        assert_eq!(
            drawn_glyph('\u{2573}', cell).expect("drawn").len(),
            2,
            "a cross is both diagonals"
        );
    }

    #[test]
    fn a_diagonal_keeps_the_weight_of_a_light_rule() {
        let cell = CellSize {
            width: 10,
            height: 20,
        };
        let stroke = Pen::for_cell(cell).width(Weight::Light) as f32;
        let outline = polygon('\u{2571}', cell);
        // Half of the stroke's width along a row reaches past the centre line.
        let top_edge = 10.0 - outline[0].0;
        let expected = stroke * 10.0_f32.hypot(20.0) / 20.0 / 2.0;
        assert!((top_edge - expected).abs() < 0.001);
    }

    fn rows_covered(character: char, cell: CellSize) -> Vec<bool> {
        covered(character, cell)
            .iter()
            .map(|row| row.iter().all(|on| *on))
            .collect()
    }

    #[test]
    fn halves_tile_with_neither_a_gap_nor_an_overlap() {
        for cell in [
            CELL,
            CellSize {
                width: 10,
                height: 19,
            },
            CellSize {
                width: 7,
                height: 29,
            },
        ] {
            let upper = rows_covered('\u{2580}', cell);
            let lower = rows_covered('\u{2584}', cell);
            assert!(upper.iter().zip(&lower).all(|(a, b)| a != b), "{cell:?}");
            let left = &covered('\u{258C}', cell)[0];
            let right = &covered('\u{2590}', cell)[0];
            assert!(left.iter().zip(right).all(|(a, b)| a != b), "{cell:?}");
        }
    }

    #[test]
    fn eighth_blocks_grow_in_order_up_to_the_full_block() {
        let cell = CellSize {
            width: 16,
            height: 32,
        };
        let heights: Vec<_> = (0x2581..=0x2588_u32)
            .map(|code| {
                let character = char::from_u32(code).expect("block element");
                rows_covered(character, cell)
                    .iter()
                    .filter(|on| **on)
                    .count()
            })
            .collect();
        assert_eq!(heights, [4, 8, 12, 16, 20, 24, 28, 32]);
        let widths: Vec<_> = (0x2589..=0x258F_u32)
            .map(|code| {
                let character = char::from_u32(code).expect("block element");
                covered(character, cell)[0].iter().filter(|on| **on).count()
            })
            .collect();
        assert_eq!(widths, [14, 12, 10, 8, 6, 4, 2]);
    }

    #[test]
    fn blocks_grow_from_the_side_they_are_named_for() {
        let cell = CELL;
        assert!(rows_covered('\u{2581}', cell)[10]);
        assert!(!rows_covered('\u{2581}', cell)[0]);
        assert!(rows_covered('\u{2594}', cell)[0]);
        assert!(!rows_covered('\u{2594}', cell)[10]);
        assert!(covered('\u{258F}', cell)[0][0]);
        assert!(covered('\u{2595}', cell)[0][10]);
    }

    #[test]
    fn quadrants_cover_only_the_corners_they_name() {
        let corners = |character| {
            let grid = covered(character, CELL);
            [grid[0][0], grid[0][10], grid[10][0], grid[10][10]]
        };
        assert_eq!(corners('\u{2596}'), [false, false, true, false]);
        assert_eq!(corners('\u{2597}'), [false, false, false, true]);
        assert_eq!(corners('\u{2598}'), [true, false, false, false]);
        assert_eq!(corners('\u{2599}'), [true, false, true, true]);
        assert_eq!(corners('\u{259A}'), [true, false, false, true]);
        assert_eq!(corners('\u{259B}'), [true, true, true, false]);
        assert_eq!(corners('\u{259C}'), [true, true, false, true]);
        assert_eq!(corners('\u{259D}'), [false, true, false, false]);
        assert_eq!(corners('\u{259E}'), [false, true, true, false]);
        assert_eq!(corners('\u{259F}'), [false, true, true, true]);
    }

    #[test]
    fn shades_darken_in_order_and_fill_the_cell() {
        let opacity = |character| match drawn_glyph(character, CELL).expect("drawn").as_slice() {
            [GlyphShape::Shade(shade)] => shade.opacity(),
            other => panic!("expected a shade, got {other:?}"),
        };
        assert!(opacity('\u{2591}') < opacity('\u{2592}'));
        assert!(opacity('\u{2592}') < opacity('\u{2593}'));
    }

    #[test]
    fn a_full_block_leaves_no_pixel_uncovered() {
        for cell in [
            CELL,
            CellSize {
                width: 1,
                height: 1,
            },
        ] {
            assert!(covered('\u{2588}', cell).iter().flatten().all(|on| *on));
        }
    }

    #[test]
    fn every_character_in_the_ranges_draws_something_and_stays_in_a_tiny_cell() {
        let cell = CellSize {
            width: 1,
            height: 1,
        };
        for code in (0x2500..=0x259F_u32).chain(0xE0B0..=0xE0B3) {
            let character = char::from_u32(code).expect("valid");
            assert!(is_drawn(character));
            let roomy = CellSize {
                width: 12,
                height: 24,
            };
            for cell in [cell, roomy] {
                for shape in drawn_glyph(character, cell).expect("drawn") {
                    match shape {
                        GlyphShape::Rect(rect) => assert!(
                            rect.x + rect.width <= cell.width
                                && rect.y + rect.height <= cell.height
                        ),
                        GlyphShape::Polygon(outline) => {
                            for (x, y) in outline {
                                assert!(
                                    (0.0..=cell.width as f32).contains(&x)
                                        && (0.0..=cell.height as f32).contains(&y),
                                    "U+{code:04X} leaves its cell at ({x}, {y})"
                                );
                            }
                        }
                        GlyphShape::Shade(_) => {}
                    }
                }
            }
            assert!(
                !drawn_glyph(character, roomy).expect("drawn").is_empty(),
                "U+{code:04X} draws nothing"
            );
        }
    }

    #[test]
    fn pieces_of_a_picture_are_graphics_and_text_is_not() {
        for character in [
            '\u{2500}',
            '\u{2588}',
            '\u{2801}',
            '\u{E0B0}',
            '\u{E0B4}',
            '\u{E0BC}',
            '\u{E0D7}',
            '\u{1CD00}',
            '\u{1FB00}',
        ] {
            assert!(is_graphic(character), "U+{:04X}", u32::from(character));
        }
        // Powerline's own branch and lock icons are read like text.
        for character in [
            'a',
            '\u{00E9}',
            '\u{25A0}',
            '\u{E0A0}',
            '\u{E0D8}',
            '\u{1F600}',
        ] {
            assert!(!is_graphic(character), "U+{:04X}", u32::from(character));
        }
    }

    #[test]
    fn everything_else_is_left_to_the_font() {
        for character in ['a', ' ', '\u{24FF}', '\u{25A0}', '\u{E0B4}'] {
            assert!(!is_drawn(character));
            assert!(drawn_glyph(character, CELL).is_none());
        }
    }
}
