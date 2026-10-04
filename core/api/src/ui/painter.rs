//! The commands of a painter, as `QPainter` has them: a module records them on its thread, the
//! core replays them on the interface thread.

use crate::egui;

/// The largest text drawn, in pixels: egui builds the glyphs of each size it is asked for.
pub const MAX_TEXT: f32 = 512.0;

/// The size in pixels to draw a text of `size` at, from 1 to `MAX_TEXT`; none for a size that
/// is not a number or is infinite.
pub fn text_pixels(size: f32) -> Option<f32> {
    size.is_finite().then(|| size.clamp(1.0, MAX_TEXT))
}

/// One command of a painter. Colours are 0xRRGGBBAA; positions are in the painting area, in
/// points, through the current transform.
#[derive(Clone, Debug, PartialEq)]
pub enum PaintCommand {
    SetPen {
        color: u32,
        width: f32,
    },
    SetBrush {
        color: u32,
    },
    DrawLine {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    DrawRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
    },
    DrawEllipse {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    DrawText {
        x: f32,
        y: f32,
        text: String,
        size: f32,
    },
    Translate {
        dx: f32,
        dy: f32,
    },
    Scale {
        sx: f32,
        sy: f32,
    },
    Save,
    Restore,
}

/// A colour of the interface objects, from 0xRRGGBBAA.
pub fn color(rgba: u32) -> egui::Color32 {
    let [r, g, b, a] = rgba.to_be_bytes();
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

#[derive(Clone, Copy)]
struct State {
    pen: egui::Stroke,
    brush: egui::Color32,
    offset: egui::Vec2,
    scale: egui::Vec2,
}

/// Draws a picture into `rect`.
pub fn replay(painter: &egui::Painter, rect: egui::Rect, picture: &[PaintCommand]) {
    let mut state = State {
        pen: egui::Stroke::new(1.0, egui::Color32::BLACK),
        brush: egui::Color32::TRANSPARENT,
        offset: rect.min.to_vec2(),
        scale: egui::vec2(1.0, 1.0),
    };
    let mut saved = Vec::new();
    let at = |state: &State, x: f32, y: f32| egui::pos2(x * state.scale.x, y * state.scale.y) + state.offset;
    let sized = |state: &State, w: f32, h: f32| egui::vec2(w * state.scale.x, h * state.scale.y);
    for command in picture {
        match command {
            PaintCommand::SetPen { color: c, width } => state.pen = egui::Stroke::new(*width, color(*c)),
            PaintCommand::SetBrush { color: c } => state.brush = color(*c),
            PaintCommand::DrawLine { x1, y1, x2, y2 } => {
                painter.line_segment([at(&state, *x1, *y1), at(&state, *x2, *y2)], state.pen);
            }
            PaintCommand::DrawRect {
                x,
                y,
                width,
                height,
                radius,
            } => {
                let shape = egui::Rect::from_min_size(at(&state, *x, *y), sized(&state, *width, *height));
                painter.rect(shape, *radius, state.brush, state.pen, egui::StrokeKind::Inside);
            }
            PaintCommand::DrawEllipse { x, y, width, height } => {
                let size = sized(&state, *width, *height);
                let center = at(&state, *x, *y) + size / 2.0;
                painter.add(egui::Shape::Ellipse(egui::epaint::EllipseShape {
                    center,
                    radius: size / 2.0,
                    fill: state.brush,
                    stroke: state.pen,
                    angle: 0.0,
                }));
            }
            PaintCommand::DrawText { x, y, text, size } => {
                if let Some(pixels) = text_pixels(size * state.scale.y) {
                    let font = egui::FontId::proportional(pixels);
                    painter.text(at(&state, *x, *y), egui::Align2::LEFT_TOP, text, font, state.pen.color);
                }
            }
            PaintCommand::Translate { dx, dy } => state.offset += sized(&state, *dx, *dy),
            PaintCommand::Scale { sx, sy } => state.scale = egui::vec2(state.scale.x * sx, state.scale.y * sy),
            PaintCommand::Save => saved.push(state),
            PaintCommand::Restore => {
                if let Some(previous) = saved.pop() {
                    state = previous;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_TEXT, color, text_pixels};

    #[test]
    fn texts_are_drawn_from_1_to_512_pixels_and_never_at_an_infinite_size() {
        assert_eq!(text_pixels(f32::INFINITY), None);
        assert_eq!(text_pixels(f32::NAN), None);
        assert_eq!(text_pixels(1e30), Some(MAX_TEXT));
        assert_eq!(text_pixels(-3.0), Some(1.0));
        assert_eq!(text_pixels(13.0), Some(13.0));
    }

    #[test]
    fn colours_are_read_as_rrggbbaa() {
        assert_eq!(color(0xFF0000FF), crate::egui::Color32::RED);
        assert_eq!(color(0x0000FF80).a(), 0x80);
        assert_eq!(color(0x00000000), crate::egui::Color32::TRANSPARENT);
    }
}
