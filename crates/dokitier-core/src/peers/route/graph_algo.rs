use core::cmp::Ordering;
use petgraph::{
    algo::Measure,
    visit::{EdgeRef, IntoEdges, VisitMap, Visitable},
};
use std::collections::HashMap;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::{collections::BinaryHeap, hash::Hash};

/// `MinScored<K, T>` holds a score `K` and a scored object `T` in
/// a pair for use with a `BinaryHeap`.
///
/// `MinScored` compares in reverse order by the score, so that we can
/// use `BinaryHeap` as a min-heap to extract the score-value pair with the
/// least score.
///
/// **Note:** `MinScored` implements a total order (`Ord`), so that it is
/// possible to use float types as scores.
#[derive(Copy, Clone, Debug)]
pub struct MinScored<K, T>(pub K, pub T);

impl<K: PartialOrd, T> PartialEq for MinScored<K, T> {
    #[inline]
    fn eq(&self, other: &MinScored<K, T>) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl<K: PartialOrd, T> Eq for MinScored<K, T> {}

impl<K: PartialOrd, T> PartialOrd for MinScored<K, T> {
    #[inline]
    fn partial_cmp(&self, other: &MinScored<K, T>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: PartialOrd, T> Ord for MinScored<K, T> {
    #[inline]
    fn cmp(&self, other: &MinScored<K, T>) -> Ordering {
        let a = &self.0;
        let b = &other.0;
        if a == b {
            Ordering::Equal
        } else if a < b {
            Ordering::Greater
        } else if a > b {
            Ordering::Less
        } else if a.ne(a) && b.ne(b) {
            // these are the NaN cases
            Ordering::Equal
        } else if a.ne(a) {
            // Order NaN less, so that it is last in the MinScore order
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }
}

pub type DijkstraResult<K, NodeId> = (HashMap<NodeId, K>, HashMap<NodeId, (NodeId, usize)>);

/// Dijkstra over a static directed edge list keyed by arbitrary node ids
/// (peer ids). Returns per-node (distance, first hop, hop count); unreachable
/// nodes are absent from the maps. Deterministic: ties pop by node id, and the
/// per-mode path stickiness on top absorbs metric jitter.
pub fn dijkstra_peer_edges<P: Copy + Eq + std::hash::Hash + Ord>(
    edges: &[(P, P, i64)],
    src: P,
) -> std::collections::HashMap<P, (i64, Option<P>, usize)> {
    dijkstra_peer_edges_with_paths(edges, src)
        .into_iter()
        .map(|(peer, (d, f, h, _))| (peer, (d, f, h)))
        .collect()
}

/// Same as [`dijkstra_peer_edges`], additionally returning the full path
/// (inclusive of both endpoints) per node. The path is required by the
/// symmetric path selection: two ends of a flow compare candidate paths by a
/// reversal-invariant canonical rank, which needs the intermediate nodes.
pub fn dijkstra_peer_edges_with_paths<P: Copy + Eq + std::hash::Hash + Ord>(
    edges: &[(P, P, i64)],
    src: P,
) -> std::collections::HashMap<P, (i64, Option<P>, usize, Vec<P>)> {
    use std::collections::{BinaryHeap, HashMap};
    use std::cmp::Reverse;

    const INF: i64 = i64::MAX / 4;
    let mut adj: HashMap<P, Vec<(P, i64)>> = HashMap::new();
    for &(from, to, weight) in edges {
        adj.entry(from).or_default().push((to, weight));
    }
    let mut dist: HashMap<P, i64> = HashMap::new();
    let mut first: HashMap<P, Option<P>> = HashMap::new();
    let mut hops: HashMap<P, usize> = HashMap::new();
    let mut parent: HashMap<P, P> = HashMap::new();
    let mut heap = BinaryHeap::new();
    dist.insert(src, 0);
    first.insert(src, Some(src));
    hops.insert(src, 0);
    heap.push((Reverse(0i64), src));
    while let Some((Reverse(d), node)) = heap.pop() {
        if dist.get(&node).copied().unwrap_or(INF) < d {
            continue;
        }
        for &(next, weight) in adj.get(&node).into_iter().flatten() {
            let nd = d + weight;
            if nd < *dist.get(&next).unwrap_or(&INF) {
                dist.insert(next, nd);
                let inherited = if node == src { Some(next) } else { first[&node] };
                first.insert(next, inherited);
                hops.insert(next, hops[&node] + 1);
                parent.insert(next, node);
                heap.push((Reverse(nd), next));
            }
        }
    }
    dist
        .into_iter()
        .map(|(peer, d)| {
            let f = first.get(&peer).copied().flatten();
            let h = hops.get(&peer).copied().unwrap_or(0);
            // Reconstruct the path by walking parents back to the source.
            let mut path = vec![peer];
            let mut cur = peer;
            while cur != src {
                match parent.get(&cur) {
                    Some(&p) => {
                        path.push(p);
                        cur = p;
                    }
                    None => break,
                }
            }
            path.reverse();
            (peer, (d, f, h, path))
        })
        .collect()
}

pub fn dijkstra_with_first_hop<G, F, K>(
    graph: G,
    start: G::NodeId,
    mut edge_cost: F,
) -> DijkstraResult<K, G::NodeId>
where
    G: IntoEdges + Visitable,
    G::NodeId: Eq + Hash + Clone,
    F: FnMut(G::EdgeRef) -> K,
    K: Measure + Copy,
{
    let mut visited = graph.visit_map();
    let mut scores = HashMap::new();
    let mut first_hop = HashMap::new();
    let mut visit_next = BinaryHeap::new();
    let zero_score = K::default();
    scores.insert(start, zero_score);
    visit_next.push(MinScored(zero_score, start));
    first_hop.insert(start, (start, 0));

    while let Some(MinScored(node_score, node)) = visit_next.pop() {
        if visited.is_visited(&node) {
            continue;
        }
        for edge in graph.edges(node) {
            let next = edge.target();
            if visited.is_visited(&next) {
                continue;
            }
            let next_score = node_score + edge_cost(edge);
            match scores.entry(next) {
                Occupied(mut ent) => {
                    if next_score < *ent.get() {
                        *ent.get_mut() = next_score;
                        visit_next.push(MinScored(next_score, next));
                        // 继承前驱的 first_hop，或自己就是第一跳
                        let hop = if node == start {
                            (next, 0)
                        } else {
                            first_hop[&node]
                        };
                        first_hop.insert(next, (hop.0, hop.1 + 1));
                    }
                }
                Vacant(ent) => {
                    ent.insert(next_score);
                    visit_next.push(MinScored(next_score, next));
                    let hop = if node == start {
                        (next, 0)
                    } else {
                        first_hop[&node]
                    };
                    first_hop.insert(next, (hop.0, hop.1 + 1));
                }
            }
        }
        visited.visit(node);
    }

    (scores, first_hop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use petgraph::graph::DiGraph;

    #[test]
    fn test_dijkstra_with_first_hop_4node() {
        let mut graph = DiGraph::<&str, u32>::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");

        graph.extend_with_edges([(a, b, 1)]);
        graph.extend_with_edges([(b, c, 1)]);
        graph.extend_with_edges([(c, d, 2)]);

        let (scores, first_hop) = dijkstra_with_first_hop(&graph, a, |edge| *edge.weight());

        assert_eq!(scores[&b], 1);
        assert_eq!(scores[&c], 2);
        assert_eq!(scores[&d], 4);

        assert_eq!(first_hop[&b], (b, 1));
        assert_eq!(first_hop[&c], (b, 2));
        assert_eq!(first_hop[&d], (b, 3));
    }

    #[test]
    fn test_dijkstra_with_first_hop() {
        let mut graph = DiGraph::<&str, u32>::new();
        let a = graph.add_node("a");
        let b = graph.add_node("b");
        let c = graph.add_node("c");
        let d = graph.add_node("d");
        let e = graph.add_node("e");

        graph.extend_with_edges([(a, b, 1), (a, c, 2), (b, d, 1), (c, d, 3), (d, e, 1)]);

        let (scores, first_hop) = dijkstra_with_first_hop(&graph, a, |edge| *edge.weight());

        assert_eq!(scores[&b], 1);
        assert_eq!(scores[&c], 2);
        assert_eq!(scores[&d], 2);
        assert_eq!(scores[&e], 3);

        assert_eq!(first_hop[&b], (b, 1));
        assert_eq!(first_hop[&c], (c, 1));
        assert_eq!(first_hop[&d], (b, 2)); // d is reached via b
        assert_eq!(first_hop[&e], (b, 3)); // e is reached via d
    }

    #[test]
    fn dijkstra_peer_edges_tracks_distance_first_hop_and_hops() {
        // a -> b(1), a -> c(2), b -> d(10), c -> d(2): best path to d is a-c-d.
        let edges = [
            (1u32, 2u32, 1i64),
            (1, 3, 2),
            (2, 4, 10),
            (3, 4, 2),
        ];
        let result = dijkstra_peer_edges(&edges, 1u32);

        let (dist_a, first_a, hops_a) = result[&1];
        assert_eq!((dist_a, first_a, hops_a), (0, Some(1), 0));
        let (dist_b, first_b, hops_b) = result[&2];
        assert_eq!((dist_b, first_b, hops_b), (1, Some(2), 1));
        let (dist_d, first_d, hops_d) = result[&4];
        assert_eq!((dist_d, first_d, hops_d), (4, Some(3), 2));
        // Unreachable node is absent.
        assert!(!result.contains_key(&99));
    }

    #[test]
    fn dijkstra_peer_edges_prefers_lower_total_cost() {
        // Two paths a -> d: direct expensive (100) vs relayed cheap (2 + 3).
        let edges = [
            (1u32, 2u32, 100i64),
            (1, 3, 2),
            (3, 2, 3),
        ];
        let result = dijkstra_peer_edges(&edges, 1u32);
        let (dist, first, hops) = result[&2];
        assert_eq!((dist, first, hops), (5, Some(3), 2));
    }
}
