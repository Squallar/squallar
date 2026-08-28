use egui::{Color32, Pos2, Rect, Vec2, vec2};

#[derive(Debug, Clone)]
pub struct Text {
    pub text: String,
    pub position: Pos2,
    pub font_size: f32,
    pub text_color: Color32,
    /// The colour of the outline drawn *behind* the glyphs.
    ///
    /// **This field used to be `background_color`, and the rename is the
    /// change.** Nothing ever drew a background with it: its only producer set
    /// it to a style's `text-halo-color` at half alpha, and its only consumer
    /// put it in `egui::TextFormat::background`, which fills the galley's
    /// bounding rectangle. So a style asking for a halo got a translucent box
    /// behind the whole label instead of an outline around the letters, which
    /// is what the `// TODO: Implement real halo rendering.` beside the
    /// producer was about. The field now says what it is; see
    /// [`halo_width`](Self::halo_width) for what a consumer is expected to do
    /// with it.
    pub halo_color: Color32,
    /// How far the halo extends from the glyphs, in screen points.
    ///
    /// Zero means no halo, which is a value the committed styles actually use
    /// (`watername_ocean` asks for `text-halo-width: 0`), so it is a real case
    /// and not merely the default.
    pub halo_width: f32,
    pub angle: f32,
    /// Wrap width in ems of [`font_size`](Self::font_size), from
    /// `text-max-width`.
    ///
    /// Ems rather than points because that is the unit MapLibre defines it in,
    /// and because the conversion needs the font the text layer will actually
    /// use. `None` means "do not wrap".
    pub max_width_ems: Option<f32>,
    /// Row height in ems, from `text-line-height`. `None` leaves the text
    /// layer's own row height alone.
    pub line_height_ems: Option<f32>,
}

impl Text {
    pub fn new(
        position: Pos2,
        text: String,
        font_size: f32,
        text_color: Color32,
        halo_color: Color32,
        angle: f32,
    ) -> Self {
        Self {
            position,
            text,
            font_size,
            text_color,
            halo_color,
            halo_width: 0.0,
            angle,
            max_width_ems: None,
            line_height_ems: None,
        }
    }

    /// This text with a halo `width` points wide.
    #[must_use]
    pub fn with_halo_width(mut self, width: f32) -> Self {
        self.halo_width = width;
        self
    }

    /// This text wrapped at `max_width_ems`, with rows `line_height_ems` apart.
    #[must_use]
    pub fn with_wrapping(
        mut self,
        max_width_ems: Option<f32>,
        line_height_ems: Option<f32>,
    ) -> Self {
        self.max_width_ems = max_width_ems;
        self.line_height_ems = line_height_ems;
        self
    }

    /// This label laid out: wrapped at `text-max-width`, rows spaced by
    /// `text-line-height`, and centred on its anchor.
    ///
    /// **The wrap width becomes points here and nowhere earlier.**
    /// `text-max-width` is defined in ems of the label's own size, and an em is
    /// only a number of points once the font that will lay the text out is
    /// known -- which is here and not in [`crate::mvt`].
    ///
    /// `halign` is [`egui::Align::Center`], matching MapLibre's default
    /// `text-justify`. The consequence a caller must handle is that the galley
    /// is measured about its own centre line, so `galley.rect.min.x` is
    /// negative; [`Self::shape`] takes the block's top-left corner and undoes
    /// that, and callers should place through it rather than passing a galley
    /// origin straight to [`egui::epaint::TextShape`].
    pub fn galley(&self, ctx: &egui::Context) -> std::sync::Arc<egui::Galley> {
        let mut job = egui::text::LayoutJob {
            halign: egui::Align::Center,
            ..Default::default()
        };

        if let Some(ems) = self.max_width_ems {
            job.wrap.max_width = ems * self.font_size;
        }

        job.append(
            &self.text,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(self.font_size),
                color: self.text_color,
                line_height: self.line_height_ems.map(|ems| ems * self.font_size),
                ..Default::default()
            },
        );

        ctx.fonts_mut(|fonts| fonts.layout_job(job))
    }

    /// The shape drawing `galley` with its block's top-left corner at
    /// `top_left`, haloed if this label asked for one.
    ///
    /// **The halo is the glyphs redrawn around themselves, not a filled box.**
    /// This used to be `egui::TextFormat::background` set to the halo colour at
    /// half alpha, which fills the galley's bounding rectangle -- a translucent
    /// slab behind the whole label rather than an outline following the
    /// letters. Eight offsets on a circle rather than four on the axes: at the
    /// one-point width the styles ask for, four leaves gaps on diagonal
    /// strokes. All nine draws share one `Arc<Galley>`, so the text is shaped
    /// once.
    ///
    /// An SDF or a blurred mask is what MapLibre does and would be better; it
    /// needs a glyph atlas this crate does not build, and offset draws are the
    /// standard approximation short of one.
    pub fn shape(&self, galley: std::sync::Arc<egui::Galley>, top_left: Pos2) -> egui::Shape {
        use egui::epaint::TextShape;

        // Rows carry negative offsets under `Align::Center`, so the galley's
        // own origin is not its top-left corner. Subtracting it puts the block
        // exactly where the collision box claimed it.
        let origin = top_left - galley.rect.min.to_vec2();

        let glyphs = |at: Pos2, color: Color32| -> egui::Shape {
            TextShape::new(at, galley.clone(), color)
                .with_angle(self.angle)
                .into()
        };

        if self.halo_width <= 0.0 || self.halo_color.a() == 0 {
            return glyphs(origin, self.text_color);
        }

        let mut shapes = Vec::with_capacity(9);
        let (sin, cos) = self.angle.sin_cos();

        for step in 0..8 {
            let theta = std::f32::consts::TAU * (step as f32) / 8.0;
            let (dx, dy) = (theta.cos() * self.halo_width, theta.sin() * self.halo_width);
            // Rotated with the label, so a line label's halo stays a ring
            // around the glyphs instead of being sheared off them.
            let offset = vec2(dx * cos - dy * sin, dx * sin + dy * cos);
            shapes.push(glyphs(origin + offset, self.halo_color));
        }

        shapes.push(glyphs(origin, self.text_color));
        egui::Shape::Vec(shapes)
    }
}

pub struct OrientedRect {
    corners: [Pos2; 4],
    bbox: Rect,
}

impl OrientedRect {
    pub fn new(center: Pos2, angle: f32, size: Vec2) -> Self {
        let (s, c) = angle.sin_cos();
        let half = size * 0.5;

        let ux = vec2(half.x * c, half.x * s);
        let uy = vec2(-half.y * s, half.y * c);

        let corners = [
            center - ux - uy, // top-left
            center + ux - uy, // top-right
            center + ux + uy, // bottom-right
            center - ux + uy, // bottom-left
        ];

        Self {
            corners,
            bbox: Rect::from_points(&corners),
        }
    }

    pub fn top_left(&self) -> Pos2 {
        self.corners[0]
    }

    pub fn intersects(&self, other: &OrientedRect) -> bool {
        // Checking bbox first gives huge performance boost.
        self.bbox.intersects(other.bbox) && !separated(&self.corners, &other.corners)
    }
}

/// The separating-axis test: two convex polygons are disjoint exactly when some
/// axis perpendicular to an edge of one of them separates their projections.
/// Opposite edges of a rectangle are parallel, so two of each rectangle's four
/// edge normals are redundant and four candidate axes decide a pair.
///
/// Projections that merely touch are *not* a separation. That is deliberate: it
/// is what the bounding-box test above reports for touching boxes, so an
/// axis-aligned pair gets the same answer from both.
fn separated(a: &[Pos2; 4], b: &[Pos2; 4]) -> bool {
    let axes = [a[1] - a[0], a[3] - a[0], b[1] - b[0], b[3] - b[0]];

    axes.into_iter().any(|edge| {
        let axis = edge.rot90();
        let (a_min, a_max) = project(a, axis);
        let (b_min, b_max) = project(b, axis);
        a_max < b_min || b_max < a_min
    })
}

/// Extent of a rectangle's corners along `axis`, in units of `axis`'s own
/// length.
///
/// The axis is deliberately left un-normalised. A degenerate rectangle -- zero
/// width or zero height, which a zero-size galley produces -- contributes a
/// zero-length edge, and normalising it would divide by zero. Un-normalised it
/// collapses every projection to the same value, so that axis reports "not
/// separating" and carries no weight, which is the correct answer rather than a
/// special case.
fn project(rect: &[Pos2; 4], axis: Vec2) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;

    for corner in rect {
        let d = corner.to_vec2().dot(axis);
        min = min.min(d);
        max = max.max(d);
    }

    (min, max)
}

// Tracks areas occupied by texts to avoid overlapping them.
pub struct OccupiedAreas {
    areas: Vec<OrientedRect>,
}

impl OccupiedAreas {
    pub fn new() -> Self {
        Self { areas: Vec::new() }
    }

    pub fn try_occupy(&mut self, rect: OrientedRect) -> bool {
        if !self.areas.iter().any(|existing| existing.intersects(&rect)) {
            self.areas.push(rect);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;
    use std::f32::consts::FRAC_PI_4;

    fn rect(cx: f32, cy: f32, angle: f32, w: f32, h: f32) -> OrientedRect {
        OrientedRect::new(pos2(cx, cy), angle, vec2(w, h))
    }

    /// Every expected value below was read off the `geo::Polygon`-based
    /// predicate this test module replaced, on these exact inputs, before it
    /// was deleted.
    ///
    /// For an axis-aligned pair the bounding-box test is already exact, so the
    /// separating-axis test has to agree with it case for case -- including the
    /// two touching cases, where both say "overlapping".
    #[test]
    fn axis_aligned_answers_match_the_bounding_box_exactly() {
        let a = || rect(5., 5., 0., 10., 10.);

        let cases = [
            ("disjoint", rect(25., 5., 0., 10., 10.), false),
            ("touching along an edge", rect(15., 5., 0., 10., 10.), true),
            ("touching at a corner", rect(15., 15., 0., 10., 10.), true),
            ("overlapping", rect(8., 8., 0., 10., 10.), true),
            ("one contains the other", rect(5., 5., 0., 2., 2.), true),
            ("identical", rect(5., 5., 0., 10., 10.), true),
        ];

        for (name, b, expected) in cases {
            let a = a();
            assert_eq!(a.intersects(&b), expected, "{name}");
            assert_eq!(b.intersects(&a), expected, "{name}, reversed");
            assert_eq!(
                a.intersects(&b),
                a.bbox.intersects(b.bbox),
                "{name}: axis-aligned, so the bounding box is already the exact answer"
            );
        }
    }

    #[test]
    fn a_rotated_rect_overlapping_an_axis_aligned_one_is_detected() {
        let square = rect(0., 0., 0., 4., 4.);
        let bar = rect(2.5, 0., FRAC_PI_4, 4., 1.);

        assert!(square.intersects(&bar));
        assert!(bar.intersects(&square));
    }

    /// The case that proves the axis test is doing real work: two thin bars
    /// crossed at right angles, far enough apart to be plainly disjoint, whose
    /// bounding boxes nonetheless overlap. Anything that answered from the
    /// bounding box alone would report these as colliding and suppress a label
    /// that has room to draw.
    #[test]
    fn a_rotated_pair_the_bounding_box_calls_overlapping_is_separated() {
        let a = rect(0., 0., FRAC_PI_4, 10., 1.);
        let b = rect(6., -6., -FRAC_PI_4, 10., 1.);

        assert!(
            a.bbox.intersects(b.bbox),
            "the premise: an AABB-only test would call these overlapping"
        );
        assert!(!a.intersects(&b));
        assert!(!b.intersects(&a));
    }

    /// The reason all four axes are candidates rather than two. A rectangle's
    /// two distinct edge normals point along its own length and its own width,
    /// and a rotated rect can be cleared of a neighbour along either -- past its
    /// end, or off its side. Neither case is decided by the other's axis, nor by
    /// either axis of the axis-aligned square, whose two are just `x` and `y`
    /// and are already spent by the bounding-box check.
    ///
    /// Asserted both ways round. Between the four assertions here, every one of
    /// the four slots in `separated`'s axis list is load-bearing: replacing any
    /// single one with a duplicate of its neighbour turns one of them red.
    #[test]
    fn a_rotated_rect_is_separated_along_either_of_its_own_axes() {
        let bar = || rect(0., 0., FRAC_PI_4, 10., 6.);

        for (name, square) in [
            ("past the bar's end", rect(6.5, 6.5, 0., 4., 4.)),
            ("off the bar's side", rect(-6., 6., 0., 4., 4.)),
        ] {
            let bar = bar();

            assert!(
                bar.bbox.intersects(square.bbox),
                "{name}: the premise -- an AABB-only test would call these overlapping"
            );
            assert!(!bar.intersects(&square), "{name}");
            assert!(!square.intersects(&bar), "{name}, reversed");
        }
    }

    #[test]
    fn degenerate_rects_do_not_panic() {
        let area = rect(5., 5., 0., 10., 10.);

        let point_inside = rect(5., 5., 0., 0., 0.);
        let point_outside = rect(50., 50., 0., 0., 0.);
        let segment_inside = rect(5., 5., 0., 10., 0.);
        let segment_touching = rect(15., 5., 0., 10., 0.);
        let rotated_segment = rect(5., 5., FRAC_PI_4, 10., 0.);

        assert!(area.intersects(&point_inside));
        assert!(point_inside.intersects(&area));
        assert!(!area.intersects(&point_outside));
        assert!(!point_outside.intersects(&area));
        assert!(area.intersects(&segment_inside));
        assert!(area.intersects(&segment_touching));
        assert!(area.intersects(&rotated_segment));
        assert!(point_inside.intersects(&point_inside));
        assert!(!point_inside.intersects(&point_outside));
    }

    #[test]
    fn occupied_areas_refuses_the_second_of_two_overlapping_labels() {
        let mut occupied = OccupiedAreas::new();

        assert!(occupied.try_occupy(rect(5., 5., 0., 10., 10.)));
        assert!(!occupied.try_occupy(rect(8., 8., 0., 10., 10.)));
        assert!(occupied.try_occupy(rect(25., 5., 0., 10., 10.)));
    }

    #[test]
    fn top_left_is_the_corner_the_galley_is_drawn_from() {
        let unrotated = rect(5., 5., 0., 10., 4.);
        assert_eq!(unrotated.top_left(), pos2(0., 3.));
    }
}
