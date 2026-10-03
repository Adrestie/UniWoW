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
