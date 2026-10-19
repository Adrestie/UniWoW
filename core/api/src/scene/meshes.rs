//! The triangles at rest of the models and the buildings, for the narrow phase of the picking: read
//! from the archives for the file a ray reaches, kept for the next rays within a budget of bytes,
//! the whole kept let go once it is passed (step 15.1: some 46 MB for the scope by default in
//! Dalaran, the files of other places read again when a ray reaches them).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::formats::{FileRef, Formats, Model, Wmo};
use crate::glam::Vec3;
use crate::viewport::Ray;

/// Triangles in the axes of their model, with their bounds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
    pub bounds: [[f32; 3]; 2],
}

impl Mesh {
    pub fn new(positions: Vec<Vec3>, triangles: Vec<[u32; 3]>) -> Self {
        let bounds = positions.iter().fold([[f32::MAX; 3], [f32::MIN; 3]], |[low, high], p| {
            [p.min(Vec3::from(low)).to_array(), p.max(Vec3::from(high)).to_array()]
        });
        let triangles = triangles
            .into_iter()
            .filter(|triangle| triangle.iter().all(|at| (*at as usize) < positions.len()))
            .collect();
        Self {
            positions,
            triangles,
            bounds,
        }
    }

    /// The triangles of the finest level of `model`; none for a model without a level.
    pub fn of_model(model: &Model) -> Self {
        let positions = model
            .vertices
            .iter()
            .map(|vertex| Vec3::from(vertex.position))
            .collect();
        let triangles = model
            .skins
            .first()
            .map(|skin| skin.triangles.as_chunks::<3>().0.to_vec())
            .unwrap_or_default();
        Self::new(positions, triangles)
    }

    /// The triangles of each group of `wmo`, in their order.
    pub fn of_wmo(wmo: &Wmo) -> Vec<Self> {
        wmo.groups
            .iter()
            .map(|group| {
                let positions = group.vertices.iter().map(|at| Vec3::from(*at)).collect();
                let triangles = group
                    .triangles
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|triangle| triangle.map(u32::from))
                    .collect();
                Self::new(positions, triangles)
            })
            .collect()
    }

    /// The nearest of its triangles `ray`, in its axes, meets, its distance; none where it misses.
    pub fn hits(&self, ray: &Ray) -> Option<f32> {
        ray.hits_box(self.bounds)?;
        self.triangles
            .iter()
            .filter_map(|[a, b, c]| {
                let at = |index: &u32| self.positions[*index as usize];
                ray.hits_triangle(at(a), at(b), at(c))
            })
            .min_by(f32::total_cmp)
    }

    pub fn bytes(&self) -> usize {
        self.positions.len() * 12 + self.triangles.len() * 12
    }
}

/// What a file of a model or a building gives the picking: its meshes, one for a model, one a
/// group for a building.
pub type Meshes = Arc<Vec<Mesh>>;

/// The meshes of the files a ray reached, read through `formats`, kept within `budget` bytes.
pub struct MeshCache {
    formats: Arc<dyn Formats>,
    budget: usize,
    kept: Mutex<(HashMap<FileRef, Meshes>, usize)>,
}

impl MeshCache {
    pub fn new(formats: Arc<dyn Formats>, budget: usize) -> Self {
        Self {
            formats,
            budget,
            kept: Mutex::default(),
        }
    }

    fn get(&self, file: &FileRef, read: impl FnOnce(&dyn Formats) -> Result<Vec<Mesh>, String>) -> Option<Meshes> {
        if let Some(found) = self.kept.lock().unwrap_or_else(PoisonError::into_inner).0.get(file) {
            return Some(found.clone());
        }
        // Read without the lock: another thread may read it meanwhile, once more at worst.
        let meshes: Meshes = Arc::new(read(&*self.formats).ok()?);
        let bytes: usize = meshes.iter().map(Mesh::bytes).sum();
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.1 + bytes > self.budget {
            kept.0.clear();
            kept.1 = 0;
        }
        kept.1 += bytes;
        kept.0.insert(file.clone(), meshes.clone());
        Some(meshes)
    }

    /// The mesh of the model `file`; none when it cannot be read.
    pub fn model(&self, file: &FileRef) -> Option<Meshes> {
        self.get(file, |formats| Ok(vec![Mesh::of_model(&formats.model(file)?)]))
    }

    /// The meshes of the groups of the building `file`; none when it cannot be read.
    pub fn wmo(&self, file: &FileRef) -> Option<Meshes> {
        self.get(file, |formats| Ok(Mesh::of_wmo(&formats.wmo(file)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ray_meets_the_nearest_triangle_of_a_mesh() {
        let square = |z: f32| {
            vec![
                Vec3::new(-1.0, -1.0, z),
                Vec3::new(1.0, -1.0, z),
                Vec3::new(1.0, 1.0, z),
                Vec3::new(-1.0, 1.0, z),
            ]
        };
        let mut positions = square(0.0);
        positions.extend(square(2.0));
        // Two squares, the second over the first; a triangle naming no vertex dropped.
        let mesh = Mesh::new(positions, vec![[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7], [0, 1, 99]]);
        assert_eq!(mesh.triangles.len(), 4);
        let down = Ray {
            origin: Vec3::new(0.2, 0.3, 10.0),
            direction: Vec3::NEG_Z,
        };
        assert!((mesh.hits(&down).unwrap() - 8.0).abs() < 1e-5);
        let aside = Ray {
            origin: Vec3::new(5.0, 0.0, 10.0),
            direction: Vec3::NEG_Z,
        };
        assert_eq!(mesh.hits(&aside), None);
    }
}
