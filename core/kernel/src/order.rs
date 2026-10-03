/// Orders `nodes` so that each comes after the nodes it depends on; dependencies outside `nodes`
/// are ignored. Returns the order and the nodes placed before their dependencies to break a cycle.
pub fn init_order(nodes: &[usize], depends_on: impl Fn(usize) -> Vec<usize>) -> (Vec<usize>, Vec<usize>) {
    let mut order: Vec<usize> = Vec::new();
    let mut forced = Vec::new();
    let mut remaining = nodes.to_vec();
    while !remaining.is_empty() {
        let ready = remaining.iter().position(|&node| {
            depends_on(node)
                .iter()
                .all(|dependency| order.contains(dependency) || !nodes.contains(dependency))
        });
        let next = ready.unwrap_or_else(|| {
            forced.push(remaining[0]);
            0
        });
        order.push(remaining.remove(next));
    }
    (order, forced)
}

#[cfg(test)]
mod tests {
    use super::init_order;

    #[test]
    fn providers_come_before_their_consumers() {
        // 0 waits for 2, which waits for 1.
        let depends_on = |node: usize| match node {
            0 => vec![2],
            2 => vec![1],
            _ => vec![],
        };
        let (order, forced) = init_order(&[0, 1, 2], depends_on);
        assert_eq!(order, vec![1, 2, 0]);
        assert!(forced.is_empty());
    }

    #[test]
    fn independent_features_keep_their_order() {
        let (order, forced) = init_order(&[3, 1, 2], |_| vec![]);
        assert_eq!(order, vec![3, 1, 2]);
        assert!(forced.is_empty());
    }

    #[test]
    fn a_cycle_is_broken_and_reported() {
        let depends_on = |node: usize| match node {
            0 => vec![1],
            1 => vec![0],
            _ => vec![],
        };
        let (order, forced) = init_order(&[0, 1, 2], depends_on);
        assert_eq!(order, vec![2, 0, 1]);
        assert_eq!(forced, vec![0]);
    }

    #[test]
    fn dependencies_outside_the_running_set_are_ignored() {
        let (order, forced) = init_order(&[0], |_| vec![5]);
        assert_eq!(order, vec![0]);
        assert!(forced.is_empty());
    }
}
