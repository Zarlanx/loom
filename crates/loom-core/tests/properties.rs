// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Property tests for the invariant core: fencing/requeue-lineage invariants
//! under arbitrary event interleavings, and the scheduler's selection logic.
//!
//! These are the proof that "no split-brain is reachable in the pure model"
//! (PR-03b gate): whatever order lineages are advanced in, the fence is globally
//! monotone, strictly increasing per lineage, attempt numbers march 1, 2, 3, …,
//! and at every step exactly one fence per lineage verifies — every superseded
//! fence is fenced off as stale.

use loom_core::capability::{Backend, BackendSet, IsolationTier, MemoryKind, MemoryModel, Version};
use loom_core::fencing::{FenceVerdict, LeaseBook};
use loom_core::ids::{AttemptId, AttemptNo, FenceToken, HostId, JobId, NodeId};
use loom_core::job::{BackendSelector, ResourceClaim};
use loom_core::node::{Node, NodeStatus};
use loom_core::scheduler::{
    NodeOffer, PlacementSignals, ScoreWeights, is_feasible, plan_placement, score,
};
use loom_core::time::Timestamp;
use proptest::prelude::*;

/// Number of distinct lineages the interleaving test advances.
const LINEAGES: usize = 4;

proptest! {
    /// Advancing any interleaving of lineages preserves every fencing invariant.
    #[test]
    fn fencing_invariants_hold_under_arbitrary_interleavings(
        steps in prop::collection::vec(0usize..LINEAGES, 1..150),
    ) {
        let mut book = LeaseBook::new();
        let jobs: Vec<JobId> = (0..LINEAGES).map(|i| JobId::new(format!("job-{i}"))).collect();
        // Every fence ever granted, per lineage, in grant order.
        let mut history: Vec<Vec<FenceToken>> = vec![Vec::new(); LINEAGES];
        let mut global_max: Option<u64> = None;

        for idx in steps {
            let job = &jobs[idx];
            let seq = history[idx].len() + 1;
            let attempt = AttemptId::new(format!("job-{idx}-att-{seq}"));
            let node = NodeId::new(format!("node-{idx}-{seq}"));
            let t0 = Timestamp::from_millis(0);

            let grant = if book.current_fence(job).is_some() {
                book.requeue(job.clone(), attempt, node, t0, t0, None)
            } else {
                book.grant_initial(job.clone(), attempt, node, t0, t0)
            };
            let grant = grant.map_err(|e| TestCaseError::fail(e.to_string()))?;

            // (1) Global monotonicity: the single minter only ever increases.
            if let Some(max) = global_max {
                prop_assert!(grant.fence.value() > max, "fence not globally increasing");
            }
            global_max = Some(grant.fence.value());

            // (2) Per-lineage strict increase: a requeue is strictly greater.
            if let Some(prev) = history[idx].last() {
                prop_assert!(grant.fence > *prev, "requeue fence not strictly greater");
            }

            // (3) Requeue lineage: attempt numbers are 1, 2, 3, … with no gaps.
            let expected_no = u32::try_from(seq).unwrap_or(u32::MAX);
            prop_assert_eq!(grant.attempt_no.get(), expected_no);
            prop_assert_eq!(book.attempt_no(job).map(AttemptNo::get), Some(expected_no));

            history[idx].push(grant.fence);

            // (4) No split-brain: exactly the newest fence verifies; every older
            //     fence for this lineage is fenced off as stale.
            let len = history[idx].len();
            for (i, &fence) in history[idx].iter().enumerate() {
                let verdict = book.check(job, fence);
                if i + 1 == len {
                    prop_assert_eq!(verdict, FenceVerdict::Accept);
                } else {
                    prop_assert_eq!(verdict, FenceVerdict::Stale, "superseded fence must be stale");
                }
            }

            // (5) Untouched lineages are unaffected by this step.
            for (other, other_job) in jobs.iter().enumerate() {
                if other == idx {
                    continue;
                }
                match history[other].last() {
                    Some(&fence) => {
                        prop_assert_eq!(book.check(other_job, fence), FenceVerdict::Accept);
                    }
                    None => prop_assert!(book.current_fence(other_job).is_none()),
                }
            }
        }
    }
}

/// Builds a candidate node whose varying fields drive the filter/score.
fn make_node(
    index: usize,
    available: bool,
    size_mb: u64,
    price: i64,
    reliability_milli: u16,
    cuda: Option<Version>,
) -> Node {
    Node {
        id: NodeId::new(format!("node-{index:04}")),
        host: HostId::new("h"),
        status: if available {
            NodeStatus::Available
        } else {
            NodeStatus::Leased
        },
        gpu_model: "RTX 4090".to_owned(),
        memory: MemoryModel::new(MemoryKind::Discrete, size_mb),
        // Always advertise CUDA (plus CPU) so the `Auto` claim resolves to CUDA and the
        // backend-scoped `min_cuda` floor — not backend resolution — is what gates the test.
        backends: BackendSet::from_backends(&[Backend::Cuda, Backend::Cpu]),
        driver: Version::new(2, 0, 0),
        cuda,
        isolation: IsolationTier::B,
        region: "local".to_owned(),
        reliability_milli,
        price_per_sec_micro_usd: price,
        last_heartbeat_at: Some(Timestamp::from_millis(0)),
    }
}

fn test_claim() -> ResourceClaim {
    ResourceClaim {
        min_memory_mb: 20_000,
        max_price_per_sec_micro_usd: 1_000,
        backend: BackendSelector::Auto,
        // Support CUDA only, so an `Auto` claim resolves to CUDA on the CUDA-advertising
        // candidate nodes — which is exactly what makes the `min_cuda` floor below apply.
        supported_backends: BackendSet::from_backends(&[Backend::Cuda]),
        // A CUDA floor so the property test exercises the backend-scoped `min_cuda`
        // constraint against nodes with an absent / too-old / sufficient CUDA line.
        min_cuda: Some(Version::new(12, 0, 0)),
        ..ResourceClaim::default()
    }
}

proptest! {
    /// `plan_placement` returns the max-score feasible node, tie-broken by id,
    /// and returns `None` exactly when nothing is feasible.
    #[test]
    fn plan_placement_selects_the_argmax_feasible_node(
        specs in prop::collection::vec(
            (
                any::<bool>(),
                0u64..80_000,
                0i64..2_000,
                0u16..=1_000,
                any::<bool>(),
                any::<bool>(),
                // CUDA major line: absent, below, or above the claim's `min_cuda` floor.
                prop::option::of(10u32..=13u32),
            ),
            0..24,
        ),
    ) {
        let claim = test_claim();
        let weights = ScoreWeights::default();

        let nodes: Vec<Node> = specs
            .iter()
            .enumerate()
            .map(|(i, &(available, size, price, rel, _, _, cuda_major))| {
                make_node(i, available, size, price, rel, cuda_major.map(|m| Version::new(m, 0, 0)))
            })
            .collect();
        let offers: Vec<NodeOffer> = nodes
            .iter()
            .zip(specs.iter())
            .map(|(node, &(_, _, _, _, cache_warm, data_local, _))| NodeOffer {
                node,
                signals: PlacementSignals { cache_warm, data_local },
            })
            .collect();

        let plan = plan_placement(&claim, &offers, &weights);

        let any_feasible = offers.iter().any(|o| is_feasible(&claim, o.node));
        prop_assert_eq!(plan.is_some(), any_feasible);

        if let Some(pick) = plan {
            // The winner is itself feasible.
            prop_assert!(is_feasible(&claim, pick.node));
            for offer in &offers {
                if is_feasible(&claim, offer.node) {
                    let other = score(&claim, offer.node, offer.signals, &weights);
                    // Argmax: no feasible node scores higher.
                    prop_assert!(pick.score >= other, "winner is not the max score");
                    // Deterministic tie-break: winner has the smallest id at the top score.
                    if other == pick.score {
                        prop_assert!(pick.node.id <= offer.node.id, "tie not broken by min id");
                    }
                }
            }
        }
    }
}
