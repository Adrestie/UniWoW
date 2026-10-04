//! The commands of a painter, as `QPainter` has them: a module records them on its thread, the
//! kernel replays them on the interface thread.

/// The largest text drawn, in pixels: egui builds the glyphs of each size it is asked for.
pub const MAX_TEXT: f32 = 512.0;

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
