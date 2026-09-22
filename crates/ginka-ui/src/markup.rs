//! Image-markup state shared by browser screenshots and pasted images.

/// One point in image-local logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    /// Horizontal position from the image's left edge.
    pub x: f32,
    /// Vertical position from the image's top edge.
    pub y: f32,
}

impl Point {
    /// Build a point in image-local logical pixels.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn differs_from(self, other: Self) -> bool {
        (self.x - other.x).abs() > f32::EPSILON || (self.y - other.y).abs() > f32::EPSILON
    }
}

/// Annotation tool selected in the image toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupTool {
    /// Freehand opaque stroke.
    Pen,
    /// Freehand translucent stroke.
    Highlight,
    /// Line with an arrow head at its final point.
    Arrow,
    /// Rectangle described by two opposing corners.
    Rectangle,
    /// Ellipse inscribed by two opposing corners.
    Ellipse,
    /// Text anchored at a point.
    Text,
}

impl MarkupTool {
    /// Every tool in toolbar order.
    pub const ALL: &'static [Self] = &[
        Self::Pen,
        Self::Highlight,
        Self::Arrow,
        Self::Rectangle,
        Self::Ellipse,
        Self::Text,
    ];

    /// Accessible English name; visible translations are supplied by the app.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pen => "Pen",
            Self::Highlight => "Highlight",
            Self::Arrow => "Arrow",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Text => "Text",
        }
    }
}

/// One committed or in-progress annotation.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkupShape {
    /// Freehand pen path.
    Pen(Vec<Point>),
    /// Freehand highlighter path.
    Highlight(Vec<Point>),
    /// Arrow from the first point to the second.
    Arrow([Point; 2]),
    /// Rectangle described by opposing corners.
    Rectangle([Point; 2]),
    /// Ellipse described by opposing corners.
    Ellipse([Point; 2]),
    /// Text anchored at a point.
    Text { at: Point, text: String },
}

impl MarkupShape {
    /// Geometry points used to render this shape.
    pub fn points(&self) -> &[Point] {
        match self {
            Self::Pen(points) | Self::Highlight(points) => points,
            Self::Arrow(points) | Self::Rectangle(points) | Self::Ellipse(points) => points,
            Self::Text { at, .. } => std::slice::from_ref(at),
        }
    }

    fn push(&mut self, point: Point) {
        match self {
            Self::Pen(points) | Self::Highlight(points) => {
                if points.last().is_none_or(|last| point.differs_from(*last)) {
                    points.push(point);
                }
            }
            Self::Arrow(points) | Self::Rectangle(points) | Self::Ellipse(points) => {
                points[1] = point;
            }
            Self::Text { .. } => {}
        }
    }

    fn is_meaningful(&self) -> bool {
        match self {
            Self::Pen(points) | Self::Highlight(points) => points.len() > 1,
            Self::Arrow(points) | Self::Rectangle(points) | Self::Ellipse(points) => {
                points[0].differs_from(points[1])
            }
            Self::Text { text, .. } => !text.trim().is_empty(),
        }
    }
}

/// Reversible annotation document layered over one immutable image.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MarkupDocument {
    shapes: Vec<MarkupShape>,
    active: Option<MarkupShape>,
}

impl MarkupDocument {
    /// Committed shapes in paint order.
    pub fn shapes(&self) -> &[MarkupShape] {
        &self.shapes
    }

    /// Shape currently following the pointer, if any.
    pub fn active(&self) -> Option<&MarkupShape> {
        self.active.as_ref()
    }

    /// Begin a pointer-driven shape.
    pub fn begin(&mut self, tool: MarkupTool, at: Point) {
        self.active = Some(match tool {
            MarkupTool::Pen => MarkupShape::Pen(vec![at]),
            MarkupTool::Highlight => MarkupShape::Highlight(vec![at]),
            MarkupTool::Arrow => MarkupShape::Arrow([at, at]),
            MarkupTool::Rectangle => MarkupShape::Rectangle([at, at]),
            MarkupTool::Ellipse => MarkupShape::Ellipse([at, at]),
            MarkupTool::Text => MarkupShape::Text {
                at,
                text: String::new(),
            },
        });
    }

    /// Extend the current pointer-driven shape.
    pub fn extend(&mut self, to: Point) {
        if let Some(shape) = &mut self.active {
            shape.push(to);
        }
    }

    /// Commit a meaningful shape, or discard a click without geometry.
    pub fn finish(&mut self, at: Point) -> bool {
        let Some(mut shape) = self.active.take() else {
            return false;
        };
        shape.push(at);
        if !shape.is_meaningful() {
            return false;
        }
        self.shapes.push(shape);
        true
    }

    /// Commit text created by the text tool.
    pub fn commit_text(&mut self, at: Point, text: String) -> bool {
        let shape = MarkupShape::Text { at, text };
        if !shape.is_meaningful() {
            return false;
        }
        self.active = None;
        self.shapes.push(shape);
        true
    }

    /// Remove the newest committed shape.
    pub fn undo(&mut self) -> bool {
        self.shapes.pop().is_some()
    }

    /// Remove all committed and in-progress marks.
    pub fn clear(&mut self) {
        self.shapes.clear();
        self.active = None;
    }
}

/// Compose an immutable image and its committed annotations into one
/// self-contained SVG attachment.
///
/// Only image data URLs are accepted so page-controlled URLs cannot turn the
/// attachment into an external fetch when an agent or preview opens it.
pub fn compose_svg(
    image_data_url: &str,
    width: f32,
    height: f32,
    document: &MarkupDocument,
) -> Option<String> {
    const SAFE_PREFIXES: &[&str] = &[
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/gif;base64,",
        "data:image/webp;base64,",
    ];
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || !SAFE_PREFIXES
            .iter()
            .any(|prefix| image_data_url.starts_with(prefix))
    {
        return None;
    }

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="#ff4d67"/></marker></defs><image href="{}" width="{width}" height="{height}" preserveAspectRatio="none"/>"##,
        xml_escape(image_data_url)
    );
    for shape in document.shapes() {
        svg.push_str(&shape_svg(shape));
    }
    svg.push_str("</svg>");
    Some(svg)
}

fn shape_svg(shape: &MarkupShape) -> String {
    match shape {
        MarkupShape::Pen(points) => format!(
            r##"<polyline points="{}" fill="none" stroke="#ff4d67" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>"##,
            svg_points(points)
        ),
        MarkupShape::Highlight(points) => format!(
            r##"<polyline points="{}" fill="none" stroke="#ffd84d" stroke-opacity="0.42" stroke-width="14" stroke-linecap="round" stroke-linejoin="round"/>"##,
            svg_points(points)
        ),
        MarkupShape::Arrow([start, end]) => format!(
            r##"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="#ff4d67" stroke-width="3" stroke-linecap="round" marker-end="url(#arrow)"/>"##,
            start.x, start.y, end.x, end.y
        ),
        MarkupShape::Rectangle([start, end]) => {
            let (x, y, width, height) = opposing_corners(*start, *end);
            format!(
                r##"<rect x="{x}" y="{y}" width="{width}" height="{height}" fill="none" stroke="#ff4d67" stroke-width="3"/>"##
            )
        }
        MarkupShape::Ellipse([start, end]) => {
            let (x, y, width, height) = opposing_corners(*start, *end);
            format!(
                r##"<ellipse cx="{}" cy="{}" rx="{}" ry="{}" fill="none" stroke="#ff4d67" stroke-width="3"/>"##,
                x + width / 2.0,
                y + height / 2.0,
                width / 2.0,
                height / 2.0
            )
        }
        MarkupShape::Text { at, text } => format!(
            r##"<text x="{}" y="{}" fill="#ff4d67" font-family="sans-serif" font-size="18" font-weight="600">{}</text>"##,
            at.x,
            at.y,
            xml_escape(text)
        ),
    }
}

fn svg_points(points: &[Point]) -> String {
    points
        .iter()
        .map(|point| format!("{},{}", point.x, point.y))
        .collect::<Vec<_>>()
        .join(" ")
}

fn opposing_corners(start: Point, end: Point) -> (f32, f32, f32, f32) {
    (
        start.x.min(end.x),
        start.y.min(end.y),
        (start.x - end.x).abs(),
        (start.y - end.y).abs(),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_tracks_pointer_moves_and_drops_zero_length_marks() {
        let mut document = MarkupDocument::default();
        document.begin(MarkupTool::Pen, Point::new(10.0, 12.0));
        document.extend(Point::new(14.0, 18.0));
        document.finish(Point::new(20.0, 21.0));
        assert_eq!(document.shapes().len(), 1);
        assert_eq!(document.shapes()[0].points().len(), 3);

        document.begin(MarkupTool::Arrow, Point::new(4.0, 4.0));
        document.finish(Point::new(4.0, 4.0));
        assert_eq!(document.shapes().len(), 1);
    }

    #[test]
    fn undo_and_clear_operate_on_committed_shapes_only() {
        let mut document = MarkupDocument::default();
        document.begin(MarkupTool::Rectangle, Point::new(1.0, 2.0));
        document.finish(Point::new(8.0, 9.0));
        document.begin(MarkupTool::Highlight, Point::new(3.0, 3.0));
        document.extend(Point::new(6.0, 6.0));
        document.finish(Point::new(9.0, 9.0));
        assert_eq!(document.shapes().len(), 2);
        assert!(document.undo());
        assert_eq!(document.shapes().len(), 1);
        document.clear();
        assert!(document.shapes().is_empty());
        assert!(!document.undo());
    }

    #[test]
    fn every_annotation_tool_has_an_accessible_name() {
        for tool in MarkupTool::ALL {
            assert!(!tool.label().is_empty());
        }
    }

    #[test]
    fn annotations_compose_with_the_original_image_as_a_self_contained_svg() {
        let mut document = MarkupDocument::default();
        document.begin(MarkupTool::Arrow, Point::new(10.0, 20.0));
        document.finish(Point::new(80.0, 90.0));
        document.begin(MarkupTool::Rectangle, Point::new(4.0, 5.0));
        document.finish(Point::new(40.0, 50.0));
        document.commit_text(Point::new(12.0, 16.0), "Fix <this> & that".into());

        let svg = compose_svg("data:image/png;base64,aGk=", 100.0, 120.0, &document).unwrap();
        assert!(svg.contains("data:image/png;base64,aGk="));
        assert!(svg.contains("marker-end=\"url(#arrow)\""));
        assert!(svg.contains("<rect"));
        assert!(svg.contains("Fix &lt;this&gt; &amp; that"));
    }
}
