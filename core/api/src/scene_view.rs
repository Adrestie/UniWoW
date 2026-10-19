//! The views of the scene other modules' interfaces hold (step 15.12b): the objects `HierarchyView`,
//! drawn by the module `hierarchy`, and `InspectorView`, drawn by the module `properties`, through
//! the services they provide. The kernel draws them as it draws the property grids.

use std::sync::Arc;

use crate::{ServiceKey, egui};

/// What draws a view of the scene inside a module's interface.
pub trait SceneView: Send + Sync {
    /// Draws the view `id` in `ui`: of the entities `entities` (ids as text), the selection when
    /// none; the Hierarchy ignores them.
    fn show(&self, ui: &mut egui::Ui, id: egui::Id, entities: &[String]);

    /// Forgets what it kept for the view `id`, gone.
    fn forget(&self, id: egui::Id);
}

/// The service drawing the objects `HierarchyView`.
pub const HIERARCHY: ServiceKey<Arc<dyn SceneView>> = ServiceKey::new("hierarchy-view");

/// The service drawing the objects `InspectorView`.
pub const INSPECTOR: ServiceKey<Arc<dyn SceneView>> = ServiceKey::new("inspector-view");
