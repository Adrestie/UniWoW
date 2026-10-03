use std::collections::HashMap;

/// A running feature and the services it requires.
pub struct Need<'a> {
    pub id: &'a str,
    pub requires: &'a [String],
}

/// The features to block, with the first service each lacks. Blocking a feature withdraws the
/// services it provides, which may block others in turn. `providers` maps each service to the
/// feature providing it.
pub fn blocked(needs: &[Need], providers: &HashMap<String, String>) -> Vec<(usize, String)> {
    let mut providers = providers.clone();
    let mut blocked: Vec<(usize, String)> = Vec::new();
    loop {
        let mut changed = false;
        for (index, need) in needs.iter().enumerate() {
            if blocked.iter().any(|(b, _)| *b == index) {
                continue;
            }
            if let Some(service) = need.requires.iter().find(|s| !providers.contains_key(*s)) {
                blocked.push((index, service.clone()));
                providers.retain(|_, provider| provider != need.id);
                changed = true;
            }
        }
        if !changed {
            return blocked;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Need, blocked};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn providers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(s, p)| (s.to_string(), p.to_string())).collect()
    }

    #[test]
    fn satisfied_requirements_block_nothing() {
        let requires = strings(&["viewport"]);
        let needs = [Need {
            id: "cube",
            requires: &requires,
        }];
        assert!(blocked(&needs, &providers(&[("viewport", "viewport")])).is_empty());
    }

    #[test]
    fn a_missing_service_blocks_the_feature() {
        let requires = strings(&["viewport"]);
        let needs = [Need {
            id: "cube",
            requires: &requires,
        }];
        assert_eq!(blocked(&needs, &providers(&[])), vec![(0, "viewport".to_owned())]);
    }

    #[test]
    fn blocking_a_provider_blocks_its_consumers() {
        // "terrain" provides "heights" but lacks "viewport"; "spawns" requires "heights".
        let terrain = strings(&["viewport"]);
        let spawns = strings(&["heights"]);
        let needs = [
            Need {
                id: "spawns",
                requires: &spawns,
            },
            Need {
                id: "terrain",
                requires: &terrain,
            },
        ];
        let result = blocked(&needs, &providers(&[("heights", "terrain")]));
        assert_eq!(result, vec![(1, "viewport".to_owned()), (0, "heights".to_owned())]);
    }
}
