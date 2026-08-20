//! Egui implementation of [`rustdar_overlays::render::draw::PointPainter`].

use egui::{Color32, FontId, Pos2, Shape, Stroke};
use rustdar_overlays::render::draw::{PointPainter, TextAnchor};

pub(crate) struct EguiPointPainter<'a> {
    pub painter: &'a egui::Painter,
    pub center: Pos2,
}

impl EguiPointPainter<'_> {
    fn pos(&self, offset: [f32; 2]) -> Pos2 {
        Pos2::new(self.center.x + offset[0], self.center.y + offset[1])
    }

    fn color(rgba: [u8; 4]) -> Color32 {
        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
    }
}

impl PointPainter for EguiPointPainter<'_> {
    fn circle_filled(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4]) {
        self.painter
            .circle_filled(self.pos(offset), radius, Self::color(color));
    }

    fn circle_stroke(&mut self, offset: [f32; 2], radius: f32, color: [u8; 4], width: f32) {
        self.painter.circle_stroke(
            self.pos(offset),
            radius,
            Stroke::new(width, Self::color(color)),
        );
    }

    fn text(
        &mut self,
        offset: [f32; 2],
        text: &str,
        color: [u8; 4],
        size: f32,
        anchor: TextAnchor,
    ) {
        let align = match anchor {
            TextAnchor::TopLeft => egui::Align2::LEFT_TOP,
            TextAnchor::TopRight => egui::Align2::RIGHT_TOP,
            TextAnchor::BottomLeft => egui::Align2::LEFT_BOTTOM,
            TextAnchor::BottomRight => egui::Align2::RIGHT_BOTTOM,
            TextAnchor::CenterLeft => egui::Align2::LEFT_CENTER,
            TextAnchor::CenterRight => egui::Align2::RIGHT_CENTER,
            TextAnchor::Center => egui::Align2::CENTER_CENTER,
            TextAnchor::CenterTop => egui::Align2::CENTER_TOP,
            TextAnchor::CenterBottom => egui::Align2::CENTER_BOTTOM,
        };
        self.painter.text(
            self.pos(offset),
            align,
            text,
            FontId::proportional(size),
            Self::color(color),
        );
    }

    fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4], width: f32) {
        self.painter.line_segment(
            [self.pos(from), self.pos(to)],
            Stroke::new(width, Self::color(color)),
        );
    }

    fn filled_polygon(&mut self, points: &[[f32; 2]], color: [u8; 4]) {
        if points.len() < 3 {
            return;
        }
        let vertices: Vec<Pos2> = points.iter().map(|p| self.pos(*p)).collect();
        self.painter.add(Shape::convex_polygon(
            vertices,
            Self::color(color),
            Stroke::NONE,
        ));
    }
}
