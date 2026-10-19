//! The lights of a map as entities of the scene: its lights of `Light.dbc`, a sphere each but its
//! global one, and its zones of light of Wow.exe, an outline each.

use uniwow_api::formats::{LightRecord, TILE, ZoneLightRecord};
use uniwow_api::scene::{self, Entity, EntityId, Piece, PieceBuilder, flags};

/// The middle of the world, and the yards of a unit of `Light.dbc`.
const MIDDLE: f32 = 32.0 * TILE;
const UNIT: f32 = 36.0;

/// The lights and the zones of light of the map `map`, of `lights` and `zones`: a light at its
/// centre, its bounds its outer radius; the global light of the map, at 0, 0, 0 in the table, at
/// the origin of the world; a zone at the middle of its outline, named by the light it gives.
pub fn piece(map: u32, lights: &[LightRecord], zones: &[ZoneLightRecord]) -> Piece {
    let mut builder = PieceBuilder::new(Vec::new());
    for light in lights.iter().filter(|light| light.map == map) {
        let global = light.position == [0.0; 3];
        let [x, y, z] = light.position;
        let origin = if global {
            [0.0; 3]
        } else {
            [MIDDLE - z / UNIT, MIDDLE - x / UNIT, y / UNIT]
        };
        let label = if global {
            "Global light".to_owned()
        } else {
            String::new()
        };
        builder.push(Entity {
            id: EntityId::light(light.id),
            flags: flags::CLIENT,
            parent: None,
            label: &label,
            extra: light.params[0],
            origin,
            bounds: scene::box_around(origin, if global { 1.0 } else { light.radii[1] / UNIT }),
        });
    }
    for (index, zone) in zones.iter().filter(|zone| zone.map == map).enumerate() {
        let (low, high) = zone
            .points
            .iter()
            .fold(([f32::MAX; 2], [f32::MIN; 2]), |(low, high), [x, y]| {
                ([low[0].min(*x), low[1].min(*y)], [high[0].max(*x), high[1].max(*y)])
            });
        if low[0] > high[0] {
            continue;
        }
        let origin = [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0, 0.0];
        let label = format!("Zone of light {index} (light {})", zone.light);
        builder.push(Entity {
            id: EntityId::light_zone(map, index as u32),
            flags: flags::CLIENT,
            parent: None,
            label: &label,
            extra: zone.light,
            origin,
            bounds: [[low[0], low[1], -1000.0], [high[0], high[1], 1000.0]],
        });
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lights_of_a_map_and_its_zones_are_entities() {
        let lights = [
            LightRecord {
                id: 1,
                map: 0,
                position: [0.0; 3],
                radii: [0.0; 2],
                params: [12, 0, 0, 0, 0, 0, 0, 0],
            },
            LightRecord {
                id: 5,
                map: 0,
                position: [36.0 * 100.0, 36.0 * 50.0, 36.0 * 200.0],
                radii: [36.0 * 10.0, 36.0 * 20.0],
                params: [7; 8],
            },
            LightRecord {
                id: 9,
                map: 1,
                position: [1.0; 3],
                radii: [1.0; 2],
                params: [0; 8],
            },
        ];
        let zones = [ZoneLightRecord {
            map: 0,
            light: 5,
            points: vec![[10.0, 20.0], [30.0, 20.0], [30.0, 60.0]],
        }];
        let piece = piece(0, &lights, &zones);
        assert_eq!(piece.len(), 3);
        assert_eq!(piece.name(0), "Global light");
        assert_eq!(piece.name(1), "Light 5");
        assert_eq!(piece.origins[1], [MIDDLE - 200.0, MIDDLE - 100.0, 50.0]);
        assert_eq!(piece.bounds[1][1][0] - piece.origins[1][0], 20.0);
        assert_eq!(piece.ids[2], EntityId::light_zone(0, 0));
        assert_eq!(piece.origins[2][..2], [20.0, 40.0]);
    }
}
