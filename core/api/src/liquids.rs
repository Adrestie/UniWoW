//! Interface of the "liquids" service: the surfaces of the water the module `liquids` holds, by
//! which a layer tells whether what it blends lies beyond the surface from the eye or on its side
//! (`viewport::Phase`). They are kept by tile of the map, each tile a grid of its cells of liquid,
//! made by the job reading the tile, so that giving them anew shares those grids.

use std::collections::HashMap;
use std::sync::Arc;

use crate::formats::{CHUNK, ORIGIN};
use crate::viewport::Phase;
use crate::{ServiceKey, glam};

/// Provide with `Registrar::provide(SERVICE, …)`, ask with `Context::service(SERVICE)`.
pub const SERVICE: ServiceKey<Handle> = ServiceKey::new("liquids");

pub type Handle = Arc<dyn Liquids>;

/// The side of a cell of liquid, 8 of a chunk of terrain, in yards.
pub const CELL: f32 = CHUNK / 8.0;
/// The cells of liquid on a side of a tile of the map: 16 chunks of 8.
pub const SIDE: i32 = 128;

/// The liquids held: callable from any thread.
pub trait Liquids: Send + Sync {
    /// The surfaces of the water as they are now; the same until the liquids held change, so that
    /// a layer takes them once a frame.
    fn surfaces(&self) -> Arc<Surfaces>;
}

/// The water over a tile of the map: one height over all its cells, or the height over each, NaN
/// where it has none.
#[derive(Clone, Debug, PartialEq)]
pub enum Grid {
    Flat(f32),
    Cells(Box<[f32]>),
}

impl Grid {
    /// The water of the tile of the map `tile` (`Surfaces::tile`) over the cells `cells` at their
    /// heights, the highest where a cell is given several, cells of other tiles left out; none
    /// without any.
    pub fn of(tile: [i32; 2], cells: impl IntoIterator<Item = ([i32; 2], f32)>) -> Option<Self> {
        let mut heights = vec![f32::NAN; (SIDE * SIDE) as usize];
        let mut any = false;
        for (cell, height) in cells {
            let (row, column) = (cell[0] - tile[0] * SIDE, cell[1] - tile[1] * SIDE);
            if !(0..SIDE).contains(&row) || !(0..SIDE).contains(&column) {
                continue;
            }
            let at = &mut heights[(row * SIDE + column) as usize];
            *at = if at.is_nan() { height } else { at.max(height) };
            any = true;
        }
        if !any {
            return None;
        }
        let first = heights[0];
        Some(if heights.iter().all(|height| *height == first) {
            Grid::Flat(first)
        } else {
            Grid::Cells(heights.into_boxed_slice())
        })
    }

    /// The height of the water over the cell of `row` and `column` of the tile; none where none.
    fn at(&self, row: i32, column: i32) -> Option<f32> {
        match self {
            Grid::Flat(height) => Some(*height),
            Grid::Cells(heights) => Some(heights[(row * SIDE + column) as usize]).filter(|height| !height.is_nan()),
        }
    }

    /// What it takes in memory.
    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + match self {
                Grid::Flat(_) => 0,
                Grid::Cells(heights) => heights.len() * 4,
            }
    }
}

/// The surfaces of the water, magma and slime aside: the height of the water over each of its cells,
/// the highest where layers overlap, by tile of the map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Surfaces {
    tiles: HashMap<[i32; 2], Arc<Grid>>,
}

impl Surfaces {
    /// The place of the cell of liquid over the point `x`, `y` of the world.
    pub fn cell(x: f32, y: f32) -> [i32; 2] {
        [
            ((ORIGIN - x) / CELL).floor() as i32,
            ((ORIGIN - y) / CELL).floor() as i32,
        ]
    }

    /// The tile of the map holding the cell `cell`: of the file `<x>_<y>`, `[y, x]`.
    pub fn tile(cell: [i32; 2]) -> [i32; 2] {
        [cell[0].div_euclid(SIDE), cell[1].div_euclid(SIDE)]
    }

    /// The water `grid` over the tile `tile`.
    pub fn insert(&mut self, tile: [i32; 2], grid: Arc<Grid>) {
        self.tiles.insert(tile, grid);
    }

    /// The water over `cells` at their heights, the highest where a cell is given several.
    pub fn from_cells(cells: impl IntoIterator<Item = ([i32; 2], f32)>) -> Self {
        let mut by_tile: HashMap<[i32; 2], Vec<([i32; 2], f32)>> = HashMap::new();
        for (cell, height) in cells {
            by_tile.entry(Self::tile(cell)).or_default().push((cell, height));
        }
        Self {
            tiles: by_tile
                .into_iter()
                .filter_map(|(tile, cells)| Some((tile, Arc::new(Grid::of(tile, cells)?))))
                .collect(),
        }
    }

    /// The tiles of the map with water.
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// The height of the water over the point `x`, `y`; none where there is none.
    pub fn surface(&self, x: f32, y: f32) -> Option<f32> {
        let cell = Self::cell(x, y);
        let tile = Self::tile(cell);
        self.tiles
            .get(&tile)?
            .at(cell[0] - tile[0] * SIDE, cell[1] - tile[1] * SIDE)
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
    use crate::formats::TILE;
    use crate::glam::Vec3;

    #[test]
    fn a_blended_batch_is_beyond_the_surface_when_it_lies_on_its_other_side_from_the_eye() {
        let over = Surfaces::cell(100.0, 200.0);
        let surfaces = Surfaces::from_cells([(over, 10.0), (over, 12.0), (over, 11.0)]);
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
        // The cells of a chunk: 8 a side, the chunk's corner of highest X and Y their first.
        assert_eq!(Surfaces::cell(ORIGIN - 0.5, ORIGIN - CELL - 0.5), [0, 1]);
    }

    #[test]
    fn the_water_of_a_tile_is_one_height_where_it_covers_it_all_at_one_and_a_grid_otherwise() {
        // The tile of the file 31_49: its first cell at the corner of highest X and Y.
        let tile = [49, 31];
        let first = [49 * SIDE, 31 * SIDE];
        let all = (0..SIDE).flat_map(|row| (0..SIDE).map(move |column| ([first[0] + row, first[1] + column], -1.0)));
        assert_eq!(Grid::of(tile, all.clone()), Some(Grid::Flat(-1.0)), "an ocean");
        let shore = all.filter(|(cell, _)| cell[1] < first[1] + 64);
        let Some(Grid::Cells(heights)) = Grid::of(tile, shore.clone()) else {
            panic!("half covered");
        };
        assert_eq!((heights[63], heights[64].is_nan(), heights.len()), (-1.0, true, 16_384));
        assert_eq!(Grid::of(tile, [([0, 0], 1.0)]), None, "a cell of another tile");
        // Found by the place in the world: the corner of the tile and the cell beside its half.
        let surfaces = Surfaces::from_cells(shore);
        let corner = [ORIGIN - 49.0 * TILE - 0.5, ORIGIN - 31.0 * TILE - 0.5];
        assert_eq!(surfaces.surface(corner[0], corner[1]), Some(-1.0));
        assert_eq!(surfaces.surface(corner[0], corner[1] - 63.0 * CELL), Some(-1.0));
        assert_eq!(surfaces.surface(corner[0], corner[1] - 64.0 * CELL), None);
        assert_eq!(
            (surfaces.len(), Surfaces::tile(Surfaces::cell(corner[0], corner[1]))),
            (1, tile)
        );
    }
}
