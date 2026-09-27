//! Weighted maximum k-coverage with greedy + CELF lazy evaluation.
//!
//! Objective: each demand point i (weight w_i, e.g. Zensus population) counts
//! its weight once per coverage level until it is covered by k_target chosen
//! candidates — so k_target = 2 rewards redundancy exactly once per point.
//! This objective is monotone submodular, so greedy carries the (1 − 1/e)
//! approximation guarantee; CELF makes iterations near-free without changing
//! any pick. Hop/connectivity constraints enter as candidate filtering by the
//! caller (review §8.3: v1 optimizes within one cell).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// One placement candidate: the demand-point indices it covers (from viewshed
/// + propagation prefiltering at planning resolution). Indices must be unique
/// within one candidate.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: u32,
    pub covers: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub candidate_id: u32,
    /// Marginal weight gained by this pick.
    pub gain: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Selection {
    pub picks: Vec<Pick>,
    /// Total weight covered (each point counted min(cover_count, k) times).
    pub objective: f64,
}

struct HeapEntry {
    gain: f64,
    round: usize,
    idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.gain == other.gain && self.idx == other.idx
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.gain
            .total_cmp(&other.gain)
            .then_with(|| other.idx.cmp(&self.idx))
    }
}

fn marginal_gain(cand: &Candidate, weights: &[f32], cover_count: &[u8], k: u8) -> f64 {
    cand.covers
        .iter()
        .filter(|&&i| cover_count[i as usize] < k)
        .map(|&i| weights[i as usize] as f64)
        .sum()
}

/// Greedy weighted maximum k-coverage (CELF). Selects up to `select_n`
/// candidates; stops early when no candidate adds anything.
pub fn greedy_max_coverage(
    weights: &[f32],
    candidates: &[Candidate],
    select_n: usize,
    k_target: u8,
) -> Selection {
    greedy_max_coverage_seeded(weights, candidates, &[], select_n, k_target)
}

/// As [`greedy_max_coverage`], but seeded with sites that are already
/// deployed: their coverage counts toward the objective before the first
/// pick, so every marginal gain is measured against reality rather than an
/// empty map. A seeded run answers "where do the next N nodes go?" and
/// generally yields a DIFFERENT network from a clean-sheet run of the same
/// size — the growth path depends on the starting set.
pub fn greedy_max_coverage_seeded(
    weights: &[f32],
    candidates: &[Candidate],
    seed: &[Candidate],
    select_n: usize,
    k_target: u8,
) -> Selection {
    assert!(k_target >= 1, "k_target must be >= 1");
    let mut cover_count = vec![0u8; weights.len()];
    for s in seed {
        for &i in &s.covers {
            let c = &mut cover_count[i as usize];
            if *c < k_target {
                *c += 1;
            }
        }
    }
    let mut heap: BinaryHeap<HeapEntry> = candidates
        .iter()
        .enumerate()
        .map(|(idx, c)| HeapEntry {
            gain: marginal_gain(c, weights, &cover_count, k_target),
            round: 0,
            idx,
        })
        .collect();

    let mut selection = Selection::default();
    let mut chosen = vec![false; candidates.len()];
    let mut round = 0usize;

    while selection.picks.len() < select_n {
        let Some(top) = heap.pop() else { break };
        if chosen[top.idx] {
            continue;
        }
        if top.round == round {
            // Gain is current — accept.
            if top.gain <= 0.0 {
                break;
            }
            for &i in &candidates[top.idx].covers {
                let c = &mut cover_count[i as usize];
                if *c < k_target {
                    *c += 1;
                }
            }
            chosen[top.idx] = true;
            selection.objective += top.gain;
            selection.picks.push(Pick { candidate_id: candidates[top.idx].id, gain: top.gain });
            round += 1;
        } else {
            // Stale — recompute lazily and push back.
            let gain = marginal_gain(&candidates[top.idx], weights, &cover_count, k_target);
            heap.push(HeapEntry { gain, round, idx: top.idx });
        }
    }
    selection
}

/// Objective for a selection under the min(cover_count, k) rule.
fn objective(weights: &[f32], cover_count: &[u8], k: u8) -> f64 {
    weights
        .iter()
        .zip(cover_count)
        .map(|(&w, &c)| w as f64 * c.min(k) as f64)
        .sum()
}

fn cover_counts(
    candidates: &[Candidate],
    selected: &[usize],
    n_demand: usize,
) -> Vec<u8> {
    let mut counts = vec![0u8; n_demand];
    for &s in selected {
        for &i in &candidates[s].covers {
            counts[i as usize] = counts[i as usize].saturating_add(1);
        }
    }
    counts
}

#[derive(Debug, Clone, PartialEq)]
pub struct RefineStats {
    pub rounds: usize,
    pub swaps: usize,
    pub objective_before: f64,
    pub objective_after: f64,
}

/// Fixed-N local search: improve an existing selection of exactly N sites by
/// swapping members out for non-members, until no swap helps.
///
/// WHY THIS EXISTS (the maintainer, 2026-08-31): greedy is *nested* — its k-th pick
/// never depends on the target N, so the first 350 picks of a 1000-pick
/// greedy run are byte-identical to a 350-pick greedy run. That makes the
/// coverage curve free, but it also means every point on that curve is a
/// GROWTH PATH solution, not a from-scratch design for that N. Optimizing
/// the whole placement at a fixed N is a different problem and yields a
/// different network. This function is that second variant; the gap between
/// the two is the measurable cost of incremental deployment.
///
/// Each round recomputes exact removal-losses and insertion-gains, then
/// applies as many disjoint improving swaps as it can find (each re-verified
/// against the live cover counts before it is applied).
pub fn refine_by_swaps(
    weights: &[f32],
    candidates: &[Candidate],
    selected: &mut Vec<usize>,
    k_target: u8,
    max_rounds: usize,
    beam: usize,
) -> RefineStats {
    let n_demand = weights.len();
    let k = k_target.max(1);
    let mut counts = cover_counts(candidates, selected, n_demand);
    let objective_before = objective(weights, &counts, k);
    let mut obj = objective_before;

    // Exact delta helpers over the live counts.
    let removal_loss = |counts: &[u8], s: usize| -> f64 {
        candidates[s]
            .covers
            .iter()
            .filter(|&&i| counts[i as usize] <= k)
            .map(|&i| weights[i as usize] as f64)
            .sum()
    };
    let insertion_gain = |counts: &[u8], c: usize| -> f64 {
        candidates[c]
            .covers
            .iter()
            .filter(|&&i| counts[i as usize] < k)
            .map(|&i| weights[i as usize] as f64)
            .sum()
    };
    let apply_remove = |counts: &mut [u8], s: usize| {
        for &i in &candidates[s].covers {
            counts[i as usize] = counts[i as usize].saturating_sub(1);
        }
    };
    let apply_add = |counts: &mut [u8], c: usize| {
        for &i in &candidates[c].covers {
            counts[i as usize] = counts[i as usize].saturating_add(1);
        }
    };

    let mut in_selection = vec![false; candidates.len()];
    for &s in selected.iter() {
        in_selection[s] = true;
    }

    let mut rounds = 0usize;
    let mut swaps = 0usize;
    for _ in 0..max_rounds {
        rounds += 1;
        // Cheapest-to-lose incumbents and best-gain outsiders.
        let mut losses: Vec<(f64, usize)> =
            selected.iter().map(|&s| (removal_loss(&counts, s), s)).collect();
        losses.sort_by(|a, b| a.0.total_cmp(&b.0));
        losses.truncate(beam);

        let mut gains: Vec<(f64, usize)> = (0..candidates.len())
            .filter(|&c| !in_selection[c])
            .map(|c| (insertion_gain(&counts, c), c))
            .collect();
        gains.sort_by(|a, b| b.0.total_cmp(&a.0));
        gains.truncate(beam);

        // Apply disjoint improving swaps, re-verified against live counts.
        let mut used_out = vec![false; losses.len()];
        let mut used_in = vec![false; gains.len()];
        let mut applied_this_round = 0usize;
        loop {
            let mut best: Option<(f64, usize, usize)> = None; // (delta, li, gi)
            for (li, &(_, s)) in losses.iter().enumerate() {
                if used_out[li] {
                    continue;
                }
                // Exact: remove s, measure each candidate's gain, restore.
                apply_remove(&mut counts, s);
                let loss_exact = obj - objective(weights, &counts, k);
                for (gi, &(_, c)) in gains.iter().enumerate() {
                    if used_in[gi] || c == s {
                        continue;
                    }
                    let delta = insertion_gain(&counts, c) - loss_exact;
                    if delta > 1e-9 && best.map(|(b, _, _)| delta > b).unwrap_or(true) {
                        best = Some((delta, li, gi));
                    }
                }
                apply_add(&mut counts, s); // restore
            }
            let Some((delta, li, gi)) = best else { break };
            let (s, c) = (losses[li].1, gains[gi].1);
            apply_remove(&mut counts, s);
            apply_add(&mut counts, c);
            obj += delta;
            in_selection[s] = false;
            in_selection[c] = true;
            if let Some(slot) = selected.iter_mut().find(|x| **x == s) {
                *slot = c;
            }
            used_out[li] = true;
            used_in[gi] = true;
            swaps += 1;
            applied_this_round += 1;
        }
        if applied_this_round == 0 {
            break;
        }
    }

    // Recompute from scratch to guard against drift.
    let counts = cover_counts(candidates, selected, n_demand);
    RefineStats {
        rounds,
        swaps,
        objective_before,
        objective_after: objective(weights, &counts, k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference implementation: recompute every gain each round.
    fn greedy_naive(
        weights: &[f32],
        candidates: &[Candidate],
        select_n: usize,
        k: u8,
    ) -> Selection {
        let mut cover_count = vec![0u8; weights.len()];
        let mut chosen = vec![false; candidates.len()];
        let mut sel = Selection::default();
        for _ in 0..select_n {
            let mut best: Option<(usize, f64)> = None;
            for (idx, c) in candidates.iter().enumerate() {
                if chosen[idx] {
                    continue;
                }
                let g = marginal_gain(c, weights, &cover_count, k);
                let better = match best {
                    None => true,
                    // Tie-break on lower idx, matching CELF's ordering.
                    Some((bidx, bg)) => g > bg || (g == bg && idx < bidx),
                };
                if better {
                    best = Some((idx, g));
                }
            }
            let Some((idx, g)) = best else { break };
            if g <= 0.0 {
                break;
            }
            for &i in &candidates[idx].covers {
                let c = &mut cover_count[i as usize];
                if *c < k {
                    *c += 1;
                }
            }
            chosen[idx] = true;
            sel.objective += g;
            sel.picks.push(Pick { candidate_id: candidates[idx].id, gain: g });
        }
        sel
    }

    fn cand(id: u32, covers: &[u32]) -> Candidate {
        Candidate { id, covers: covers.to_vec() }
    }

    #[test]
    fn picks_biggest_disjoint_sets() {
        let weights = vec![1.0; 6];
        let cands = vec![cand(10, &[0, 1, 2]), cand(11, &[3, 4]), cand(12, &[5])];
        let s = greedy_max_coverage(&weights, &cands, 2, 1);
        assert_eq!(
            s.picks.iter().map(|p| p.candidate_id).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert!((s.objective - 5.0).abs() < 1e-9);
    }

    #[test]
    fn overlap_counts_marginally() {
        let weights = vec![1.0; 4];
        // A covers {0,1,2}; B covers {1,2,3}: after A, B's marginal gain is 1.
        let cands = vec![cand(1, &[0, 1, 2]), cand(2, &[1, 2, 3])];
        let s = greedy_max_coverage(&weights, &cands, 2, 1);
        assert_eq!(s.picks[0].candidate_id, 1);
        assert!((s.picks[1].gain - 1.0).abs() < 1e-9);
        assert!((s.objective - 4.0).abs() < 1e-9);
    }

    #[test]
    fn k2_rewards_redundancy() {
        // Point 0 weighs 10, point 1 weighs 1. With k=2, covering point 0
        // twice (10 + 10) beats covering point 1 once.
        let weights = vec![10.0, 1.0];
        let cands = vec![cand(1, &[0]), cand(2, &[0]), cand(3, &[1])];
        let s = greedy_max_coverage(&weights, &cands, 2, 2);
        let ids: Vec<_> = s.picks.iter().map(|p| p.candidate_id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert!((s.objective - 20.0).abs() < 1e-9);
    }

    #[test]
    fn stops_when_nothing_left_to_gain() {
        let weights = vec![1.0; 2];
        let cands = vec![cand(1, &[0, 1]), cand(2, &[0, 1])];
        let s = greedy_max_coverage(&weights, &cands, 5, 1);
        assert_eq!(s.picks.len(), 1);
    }

    /// Greedy is NESTED: its k-th pick never depends on the target N, so a
    /// long run's prefix equals a short run exactly. This is what makes the
    /// coverage curve free — and also what makes every point on it a
    /// growth-path solution rather than a fixed-N design.
    #[test]
    fn greedy_prefixes_are_nested() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut next = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        let weights: Vec<f32> = (0..200).map(|_| (next() % 50) as f32).collect();
        let candidates: Vec<Candidate> = (0..60)
            .map(|id| {
                let n_cov = 3 + (next() % 25) as usize;
                let mut covers: Vec<u32> = (0..n_cov).map(|_| next() % 200).collect();
                covers.sort_unstable();
                covers.dedup();
                Candidate { id, covers }
            })
            .collect();
        let long = greedy_max_coverage(&weights, &candidates, 12, 1);
        for n in 1..=12 {
            let short = greedy_max_coverage(&weights, &candidates, n, 1);
            assert_eq!(short.picks, long.picks[..n], "prefix mismatch at n={n}");
        }
    }

    /// A seeded run must measure gains against what is already deployed:
    /// a candidate duplicating the seed's coverage adds nothing.
    #[test]
    fn seeding_changes_the_growth_path() {
        let weights = vec![1.0f32; 6];
        let a = Candidate { id: 0, covers: vec![0, 1, 2, 3] };
        let b = Candidate { id: 1, covers: vec![0, 1, 2] };
        let c = Candidate { id: 2, covers: vec![4, 5] };
        // Clean sheet: b (3 points) beats c (2 points).
        let fresh = greedy_max_coverage(&weights, &[b.clone(), c.clone()], 1, 1);
        assert_eq!(fresh.picks[0].candidate_id, 1);
        // Seeded with a: b is redundant, so c wins instead.
        let seeded = greedy_max_coverage_seeded(&weights, &[b, c], &[a], 1, 1);
        assert_eq!(seeded.picks[0].candidate_id, 2);
        assert!((seeded.picks[0].gain - 2.0).abs() < 1e-9);
    }

    /// Fixed-N refinement must escape a greedy trap: greedy takes a broad
    /// first set that blocks the better pair, and a swap recovers it.
    #[test]
    fn refinement_improves_on_the_greedy_path() {
        // 10 demand points, all weight 1.
        let weights = vec![1.0f32; 10];
        let candidates = vec![
            // Greedy's first pick (5 points) — but it overlaps both of the
            // two disjoint 4-point sets that together would cover 8.
            Candidate { id: 0, covers: vec![0, 1, 2, 3, 4] },
            Candidate { id: 1, covers: vec![0, 1, 5, 6] },
            Candidate { id: 2, covers: vec![2, 3, 7, 8] },
        ];
        let greedy = greedy_max_coverage(&weights, &candidates, 2, 1);
        // Greedy: picks 0 (5), then best remaining adds 2 → 7 total.
        assert!((greedy.objective - 7.0).abs() < 1e-9, "{}", greedy.objective);
        let mut sel: Vec<usize> =
            greedy.picks.iter().map(|p| p.candidate_id as usize).collect();
        let stats = refine_by_swaps(&weights, &candidates, &mut sel, 1, 10, 8);
        // Swapping candidate 0 out for the other 4-set gives 8.
        assert!((stats.objective_before - 7.0).abs() < 1e-9);
        assert!((stats.objective_after - 8.0).abs() < 1e-9, "{stats:?}");
        assert_eq!(stats.swaps, 1);
        sel.sort_unstable();
        assert_eq!(sel, vec![1, 2]);
    }

    #[test]
    fn refinement_is_a_no_op_when_greedy_is_already_optimal() {
        let weights = vec![1.0f32; 6];
        let candidates = vec![
            Candidate { id: 0, covers: vec![0, 1, 2] },
            Candidate { id: 1, covers: vec![3, 4, 5] },
            Candidate { id: 2, covers: vec![0, 1] },
        ];
        let greedy = greedy_max_coverage(&weights, &candidates, 2, 1);
        let mut sel: Vec<usize> =
            greedy.picks.iter().map(|p| p.candidate_id as usize).collect();
        let stats = refine_by_swaps(&weights, &candidates, &mut sel, 1, 10, 8);
        assert_eq!(stats.swaps, 0);
        assert!((stats.objective_after - stats.objective_before).abs() < 1e-9);
        assert!((stats.objective_after - 6.0).abs() < 1e-9);
    }

    #[test]
    fn celf_matches_naive_on_random_instances() {
        // Deterministic LCG so the test needs no rand dependency.
        let mut state = 0x2545F491_4F6CDD1Du64;
        let mut next = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _case in 0..20 {
            let n_demand = 40;
            let weights: Vec<f32> = (0..n_demand).map(|_| (next() % 100) as f32 / 10.0).collect();
            let candidates: Vec<Candidate> = (0..15)
                .map(|id| {
                    let n_cov = 1 + (next() % 8) as usize;
                    let mut covers: Vec<u32> =
                        (0..n_cov).map(|_| next() % n_demand as u32).collect();
                    covers.sort_unstable();
                    covers.dedup();
                    Candidate { id, covers }
                })
                .collect();
            for k in [1u8, 2] {
                let a = greedy_max_coverage(&weights, &candidates, 6, k);
                let b = greedy_naive(&weights, &candidates, 6, k);
                assert_eq!(a.picks, b.picks, "k={k}");
                assert!((a.objective - b.objective).abs() < 1e-9);
            }
        }
    }
}
