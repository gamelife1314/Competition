//! A* pathfinding on the 8-neighbour grid; returns the next step to take.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::cmp::Reverse;

use crate::model::{chebyshev, neighbours, Turn};
use crate::protocol::Pos;

#[derive(PartialEq, Eq)]
struct Node {
    f: i32,
    cost: i32,
    seq: u32,
    pos: Pos,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.f, self.cost, self.seq).cmp(&(other.f, other.cost, other.seq))
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}


/// Walk toward `goal` but stop at any of `stands` (cells from which the goal
/// is actionable). Returns the next step to take this round, if any.
pub fn step_toward_stands(
    turn: &Turn,
    start: Pos,
    stands: &[Pos],
    blocked: &HashSet<Pos>,
) -> Option<Pos> {
    if stands.iter().any(|stand| *stand == start) {
        return None; // already at an actionable cell
    }
    let stand_set: HashSet<Pos> = stands.iter().copied().collect();
    let mut seq: u32 = 0;
    let heuristic = |pos: Pos| stands.iter().map(|stand| chebyshev(pos, *stand)).min().unwrap_or(i32::MAX);
    let mut frontier: BinaryHeap<Reverse<Node>> = BinaryHeap::new();
    frontier.push(Reverse(Node { f: heuristic(start), cost: 0, seq, pos: start }));
    let mut best: HashMap<Pos, i32> = HashMap::new();
    best.insert(start, 0);
    let mut came_from: HashMap<Pos, Pos> = HashMap::new();
    let mut seen: HashSet<Pos> = HashSet::new();

    while let Some(Reverse(current)) = frontier.pop() {
        if !seen.insert(current.pos) {
            continue;
        }
        if stand_set.contains(&current.pos) && current.pos != start {
            return Some(first_step_simple(&came_from, start, current.pos));
        }
        for step in neighbours(current.pos) {
            if !turn.is_land(step) || blocked.contains(&step) {
                continue;
            }
            let new_cost = current.cost.saturating_add(1);
            if new_cost >= *best.get(&step).unwrap_or(&i32::MAX) {
                continue;
            }
            best.insert(step, new_cost);
            came_from.insert(step, current.pos);
            seq = seq.wrapping_add(1);
            frontier.push(Reverse(Node {
                f: new_cost.saturating_add(heuristic(step)),
                cost: new_cost,
                seq,
                pos: step,
            }));
        }
    }
    None
}

fn first_step_simple(came_from: &HashMap<Pos, Pos>, start: Pos, goal: Pos) -> Pos {
    let mut current = goal;
    while let Some(prev) = came_from.get(&current) {
        if *prev == start {
            return current;
        }
        current = *prev;
    }
    current
}
