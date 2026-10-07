//! Interface of the "liquids" service: the surfaces of the water the module `liquids` holds, by
//! which a layer tells whether what it blends lies beyond the surface from the eye or on its side
//! (`viewport::Phase`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::formats::{CHUNK, ORIGIN};
use crate::viewport::Phase;
use crate::{ServiceKey, glam};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("liquids");

pub type Handle = Arc<dyn Liquids>;

/// The side of a tile of liquid, 8 of a chunk of terrain, in yards.
pub const CELL: f32 = CHUNK / 8.0;

/// The liquids held: callable from any thread.
pub trait Liquids: Send + Sync {
    /// The surfaces of the water as they are now; the same until the liquids held change, so that
    /// a layer takes them once a frame.
    fn surfaces(&self) -> Arc<Surfaces>;
}

/// The surfaces of the water, magma and slime aside: the height of the water over each of its tiles,
/// the highest where layers overlap, by the tile's place in the world.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Surfaces {
    tiles: HashMap<[i32; 2], f32>,
}

impl Surfaces {
    /// The place of the tile of liquid over the point `x`, `y` of the world.
    pub fn cell(x: f32, y: f32) -> [i32; 2] {
        [
            ((ORIGIN - x) / CELL).floor() as i32,
            ((ORIGIN - y) / CELL).floor() as i32,
        ]
    }

    /// Water over the tile `cell` at `height`, kept the highest.
    pub fn add(&mut self, cell: [i32; 2], height: f32) {
        self.tiles
            .entry(cell)
            .and_modify(|at| *at = at.max(height))
            .or_insert(height);
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The height of the water over the point `x`, `y`; none where there is none.
    pub fn surface(&self, x: f32, y: f32) -> Option<f32> {
        self.tiles.get(&Self::cell(x, y)).copied()
    }

    /// Whether `point` lies under the water.
    pub fn under(&self, point: glam::Vec3) -> bool {
        self.surface(point.x, point.y).is_some_and(|surface| point.z < surface)
    }

    /// The phase a blended batch at `point` is drawn in, seen from `eye`: beyond the surface when one
    /// of the two lies under the water and the other not, on the eye's side otherwise.
    pub fn phase(&self, eye: glam::Vec3, point: glam::Vec3) -> Phase {
        if self.under(eye) == self.under(point) {
            Phase::Near
        } else {
            Phase::Beyond
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glam::Vec3;

    #[test]
    fn a_blended_batch_is_beyond_the_surface_when_it_lies_on_its_other_side_from_the_eye() {
        let mut surfaces = Surfaces::default();
        let over = Surfaces::cell(100.0, 200.0);
        surfaces.add(over, 10.0);
        surfaces.add(over, 12.0);
        surfaces.add(over, 11.0);
        assert_eq!(surfaces.surface(100.0, 200.0), Some(12.0), "the highest");
        assert_eq!(surfaces.surface(100.0 + CELL, 200.0), None);
        let (above, below, dry) = (
            Vec3::new(100.0, 200.0, 20.0),
            Vec3::new(100.0, 200.0, 5.0),
            Vec3::new(500.0, 200.0, 5.0),
        );
        assert!(surfaces.under(below) && !surfaces.under(above) && !surfaces.under(dry));
        assert_eq!(surfaces.phase(above, below), Phase::Beyond);
        assert_eq!(surfaces.phase(above, above), Phase::Near);
        assert_eq!(surfaces.phase(above, dry), Phase::Near);
        assert_eq!(surfaces.phase(below, above), Phase::Beyond, "from under the water");
        assert_eq!(surfaces.phase(below, dry), Phase::Beyond);
        assert_eq!(surfaces.phase(below, below), Phase::Near);
        // The tiles of a chunk: 8 a side, the chunk's corner of highest X and Y their first.
        assert_eq!(Surfaces::cell(ORIGIN - 0.5, ORIGIN - CELL - 0.5), [0, 1]);
    }
}
