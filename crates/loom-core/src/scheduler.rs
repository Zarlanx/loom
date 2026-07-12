// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! The scheduler's pure decision logic: `filter` → `score` → `commit`
//! (control-plane §4).
//!
//! This module owns only the *deterministic* half — the hard-constraint filter,
//! the soft-preference score, and the selection of a winner. The single-writer
//! loop that watches the store and the fence-minting *commit* live in `loomd`
//! and [`crate::fencing`]; here everything is a pure function over inputs, so it
//! is exhaustively testable with no I/O.
//!
//! Scoring is fixed-point integer arithmetic (accumulated in `i128`), never
//! floating point: placement decisions must be deterministic and reproducible,
//! and micro-USD prices do not survive an `f64`.

use crate::capability::Backend;
use crate::job::{BackendSelector, ResourceClaim};
use crate::node::Node;

/// Per-`(job, node)` soft-preference signals the node itself does not carry
/// (control-plane §4 score): weight-cache locality and data affinity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlacementSignals {
    /// The node already holds the required weights/image (cache-warm).
    pub cache_warm: bool,
    /// The node is close to the job's input data / checkpoint store.
    pub data_local: bool,
}

/// Integer weights for the soft-preference score (control-plane §4).
///
/// Defaults encode the doc's ranking: **cache locality dominates** ("placing a
/// job where the model is already cached beats a slightly cheaper cold node"),
/// then data affinity, then reliability, then price headroom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoreWeights {
    /// Weight per micro-USD of price headroom under the ceiling (cheaper wins).
    pub price: i64,
    /// Weight per reliability milli (`0..=1000`).
    pub reliability: i64,
    /// Flat bonus when the node is cache-warm.
    pub cache_warm: i64,
    /// Flat bonus when the node is data-local.
    pub data_local: i64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            price: 1,
            reliability: 1_000,
            cache_warm: 10_000_000,
            data_local: 1_000_000,
        }
    }
}

/// A placement score. Higher is better; ordering is total (`i128`, no float).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score(pub i128);

/// A node under consideration for a job, with its soft-preference signals.
#[derive(Debug, Clone, Copy)]
pub struct NodeOffer<'a> {
    /// The candidate node.
    pub node: &'a Node,
    /// The per-`(job, node)` signals feeding the score.
    pub signals: PlacementSignals,
}

/// The scheduler's selection: which node won, which backend resolved, its score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement<'a> {
    /// The winning node.
    pub node: &'a Node,
    /// The backend the job resolves to on this node.
    pub backend: Backend,
    /// The winning score.
    pub score: Score,
}

/// Resolves which backend a job would use on a node (compute-backends.md §resolution).
///
/// - [`BackendSelector::Only`] matches only if the node advertises that backend.
/// - [`BackendSelector::Auto`] intersects the recipe's supported backends with the
///   node's, then picks the highest-priority survivor (MLX → CUDA → CPU → `ROCm`).
///
/// Returns `None` when no backend is viable on the node.
#[must_use]
pub fn resolve_backend(claim: &ResourceClaim, node: &Node) -> Option<Backend> {
    match claim.backend {
        BackendSelector::Only(backend) => node.backends.contains(backend).then_some(backend),
        BackendSelector::Auto => claim
            .supported_backends
            .intersect(node.backends)
            .best_by_priority(),
    }
}

/// The hard-constraint filter (control-plane §4). A node is feasible only if it
/// is available, can run a viable backend, and meets every resource floor.
///
/// The `min_cuda` floor is backend-scoped: it gates a node only when the job actually
/// resolves to [`Backend::Cuda`] on it (compute-backends.md / ADR-0015). A CUDA line is
/// meaningless to an MLX or CPU placement, so an [`BackendSelector::Auto`] claim that
/// resolves to MLX is never excluded by a CUDA floor.
#[must_use]
pub fn is_feasible(claim: &ResourceClaim, node: &Node) -> bool {
    let Some(backend) = resolve_backend(claim, node) else {
        return false;
    };
    is_feasible_with(claim, node, backend)
}

/// The resource-floor half of [`is_feasible`], for a node whose backend has already been
/// resolved. Split out so [`plan_placement`] can resolve the backend **once** per candidate
/// — it needs the resolved backend for the winning [`Placement`] anyway — and reuse it here,
/// instead of `is_feasible` and `plan_placement` each resolving it for the same node.
fn is_feasible_with(claim: &ResourceClaim, node: &Node, backend: Backend) -> bool {
    node.status.is_schedulable()
        && node.memory.size_mb() >= claim.min_memory_mb
        && claim
            .gpu_model
            .as_deref()
            .is_none_or(|model| node.gpu_model == model)
        && claim.min_driver.is_none_or(|floor| node.driver >= floor)
        // The CUDA floor applies only to a CUDA placement (see the doc comment).
        && (backend != Backend::Cuda
            || claim
                .min_cuda
                .is_none_or(|floor| node.cuda.is_some_and(|have| have >= floor)))
        && node.isolation.satisfies(claim.min_isolation)
        && claim
            .region_pref
            .as_deref()
            .is_none_or(|region| node.region == region)
        && node.reliability_milli >= claim.min_reliability_milli
        && node.price_per_sec_micro_usd <= claim.max_price_per_sec_micro_usd
}

/// The soft-preference score for a node (control-plane §4). Assumes the node
/// already passed [`is_feasible`], so price headroom is non-negative.
#[must_use]
pub fn score(
    claim: &ResourceClaim,
    node: &Node,
    signals: PlacementSignals,
    weights: &ScoreWeights,
) -> Score {
    let headroom = claim
        .max_price_per_sec_micro_usd
        .saturating_sub(node.price_per_sec_micro_usd)
        .max(0);
    let mut total: i128 = 0;
    total += i128::from(headroom) * i128::from(weights.price);
    total += i128::from(node.reliability_milli) * i128::from(weights.reliability);
    if signals.cache_warm {
        total += i128::from(weights.cache_warm);
    }
    if signals.data_local {
        total += i128::from(weights.data_local);
    }
    Score(total)
}

/// Runs `filter` → `score` → select over `offers`, returning the best placement.
///
/// Ties on score break deterministically by the smaller [`NodeId`](crate::ids::NodeId),
/// so the choice never depends on input order. Returns `None` when no offer is
/// feasible (the caller rejects the job at submit rather than queuing forever).
#[must_use]
pub fn plan_placement<'a>(
    claim: &ResourceClaim,
    offers: &[NodeOffer<'a>],
    weights: &ScoreWeights,
) -> Option<Placement<'a>> {
    let mut best: Option<Placement<'a>> = None;
    for offer in offers {
        // Resolve the backend once per candidate: it gates feasibility *and* is recorded on
        // the winning placement, so there is no reason to derive it twice.
        let Some(backend) = resolve_backend(claim, offer.node) else {
            continue;
        };
        if !is_feasible_with(claim, offer.node, backend) {
            continue;
        }
        let candidate = Placement {
            node: offer.node,
            backend,
            score: score(claim, offer.node, offer.signals, weights),
        };
        let wins = match &best {
            None => true,
            Some(current) => {
                candidate.score > current.score
                    || (candidate.score == current.score && candidate.node.id < current.node.id)
            }
        };
        if wins {
            best = Some(candidate);
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;
    use crate::capability::{Backend, BackendSet, IsolationTier, MemoryKind, MemoryModel, Version};
    use crate::ids::{HostId, NodeId};
    use crate::node::NodeStatus;
    use crate::time::Timestamp;

    fn node(id: &str) -> Node {
        Node {
            id: NodeId::new(id),
            host: HostId::new("h"),
            status: NodeStatus::Available,
            gpu_model: "M3 Max".to_owned(),
            memory: MemoryModel::new(MemoryKind::Unified, 48_000),
            backends: BackendSet::from_backends(&[Backend::Mlx, Backend::Cpu]),
            driver: Version::new(2, 0, 0),
            cuda: None,
            isolation: IsolationTier::B,
            region: "local".to_owned(),
            reliability_milli: 900,
            price_per_sec_micro_usd: 100,
            last_heartbeat_at: Some(Timestamp::from_millis(0)),
        }
    }

    fn claim() -> ResourceClaim {
        ResourceClaim {
            min_memory_mb: 20_000,
            max_price_per_sec_micro_usd: 1_000,
            backend: BackendSelector::Auto,
            supported_backends: BackendSet::from_backends(&[Backend::Mlx, Backend::Cuda]),
            ..ResourceClaim::default()
        }
    }

    #[test]
    fn auto_resolves_to_highest_priority_shared_backend() {
        let mut n = node("n");
        n.backends = BackendSet::from_backends(&[Backend::Cpu, Backend::Cuda, Backend::Mlx]);
        let c = ResourceClaim {
            supported_backends: BackendSet::from_backends(&[Backend::Cuda, Backend::Cpu]),
            ..claim()
        };
        // Node has mlx too, but the recipe does not support it → cuda wins.
        assert_eq!(resolve_backend(&c, &n), Some(Backend::Cuda));
    }

    #[test]
    fn explicit_backend_requires_the_node_to_advertise_it() {
        let n = node("n"); // advertises [mlx, cpu]
        let mlx = ResourceClaim {
            backend: BackendSelector::Only(Backend::Mlx),
            ..claim()
        };
        let cuda = ResourceClaim {
            backend: BackendSelector::Only(Backend::Cuda),
            ..claim()
        };
        assert_eq!(resolve_backend(&mlx, &n), Some(Backend::Mlx));
        assert_eq!(resolve_backend(&cuda, &n), None);
        assert!(!is_feasible(&cuda, &n));
    }

    #[test]
    fn each_hard_constraint_can_exclude_a_node() {
        let base = claim();

        let mut offline = node("n");
        offline.status = NodeStatus::Leased;
        assert!(!is_feasible(&base, &offline));

        let mut too_small = node("n");
        too_small.memory = MemoryModel::new(MemoryKind::Unified, 10_000);
        assert!(!is_feasible(&base, &too_small));

        let mut too_pricey = node("n");
        too_pricey.price_per_sec_micro_usd = 5_000;
        assert!(!is_feasible(&base, &too_pricey));

        let mut flaky = node("n");
        flaky.reliability_milli = 100;
        let strict = ResourceClaim {
            min_reliability_milli: 500,
            ..claim()
        };
        assert!(!is_feasible(&strict, &flaky));

        let weak_tier = ResourceClaim {
            min_isolation: IsolationTier::A,
            ..claim()
        };
        assert!(!is_feasible(&weak_tier, &node("n"))); // node is Tier B

        let wrong_region = ResourceClaim {
            region_pref: Some("us-east".to_owned()),
            ..claim()
        };
        assert!(!is_feasible(&wrong_region, &node("n"))); // node is "local"

        let old_driver = ResourceClaim {
            min_driver: Some(Version::new(9, 0, 0)),
            ..claim()
        };
        assert!(!is_feasible(&old_driver, &node("n"))); // node driver 2.0.0

        let wrong_model = ResourceClaim {
            gpu_model: Some("RTX 4090".to_owned()),
            ..claim()
        };
        assert!(!is_feasible(&wrong_model, &node("n"))); // node is "M3 Max"

        // CUDA floor: enforced only when the placement resolves to CUDA
        // (compute-backends.md / ADR-0015). Pin the claim to CUDA and give each node a
        // CUDA backend so the floor is what's under test — a node with no CUDA line, and
        // one below the floor, are excluded; a node meeting the floor passes.
        let needs_cuda = ResourceClaim {
            backend: BackendSelector::Only(Backend::Cuda),
            min_cuda: Some(Version::new(12, 0, 0)),
            ..claim()
        };
        let mut no_cuda_line = node("n");
        no_cuda_line.backends = BackendSet::from_backends(&[Backend::Cuda]);
        no_cuda_line.cuda = None;
        assert!(!is_feasible(&needs_cuda, &no_cuda_line)); // CUDA backend but no CUDA line

        let mut old_cuda = node("n");
        old_cuda.backends = BackendSet::from_backends(&[Backend::Cuda]);
        old_cuda.cuda = Some(Version::new(11, 8, 0));
        assert!(!is_feasible(&needs_cuda, &old_cuda)); // CUDA present but below the floor

        let mut ok_cuda = node("n");
        ok_cuda.backends = BackendSet::from_backends(&[Backend::Cuda]);
        ok_cuda.cuda = Some(Version::new(12, 4, 0));
        assert!(is_feasible(&needs_cuda, &ok_cuda)); // meets the CUDA floor

        // The unmodified node passes every floor.
        assert!(is_feasible(&base, &node("n")));
    }

    #[test]
    fn an_mlx_placement_ignores_the_cuda_floor() {
        // An `Auto` claim that resolves to MLX must place even with a CUDA floor set:
        // the floor gates CUDA placements only (compute-backends.md / ADR-0015). The
        // node advertises MLX and no CUDA line at all, yet a `min_cuda` claim still fits.
        let mut mlx_node = node("mlx");
        mlx_node.backends = BackendSet::from_backends(&[Backend::Mlx, Backend::Cpu]);
        mlx_node.cuda = None;
        let c = ResourceClaim {
            backend: BackendSelector::Auto,
            supported_backends: BackendSet::from_backends(&[Backend::Mlx, Backend::Cuda]),
            min_cuda: Some(Version::new(12, 0, 0)),
            ..claim()
        };
        assert_eq!(resolve_backend(&c, &mlx_node), Some(Backend::Mlx));
        assert!(
            is_feasible(&c, &mlx_node),
            "an MLX placement must ignore the CUDA floor"
        );
    }

    #[test]
    fn a_default_claim_is_feasible_on_a_plain_node() {
        // `ResourceClaim::default()` is the no-constraints claim, so it must place on any
        // schedulable node — the manual `Default` exists precisely so this holds (a
        // derived one would be unplaceable). See `job` for the field-level guard.
        assert!(is_feasible(&ResourceClaim::default(), &node("n")));
    }

    #[test]
    fn cache_warm_beats_a_cheaper_cold_node() {
        let weights = ScoreWeights::default();
        let c = claim();

        let mut cheap_cold = node("cold");
        cheap_cold.price_per_sec_micro_usd = 1; // maximal price headroom
        let mut pricey_warm = node("warm");
        pricey_warm.price_per_sec_micro_usd = 900;

        let offers = [
            NodeOffer {
                node: &cheap_cold,
                signals: PlacementSignals::default(),
            },
            NodeOffer {
                node: &pricey_warm,
                signals: PlacementSignals {
                    cache_warm: true,
                    data_local: false,
                },
            },
        ];
        let Some(pick) = plan_placement(&c, &offers, &weights) else {
            panic!("expected a placement");
        };
        assert_eq!(pick.node.id, NodeId::new("warm"));
        assert_eq!(pick.backend, Backend::Mlx);
    }

    #[test]
    fn ties_break_by_smaller_node_id() {
        let weights = ScoreWeights::default();
        let c = claim();
        // Two identical nodes (same score) with different ids.
        let a = node("aaa");
        let b = node("bbb");
        let offers = [
            NodeOffer {
                node: &b,
                signals: PlacementSignals::default(),
            },
            NodeOffer {
                node: &a,
                signals: PlacementSignals::default(),
            },
        ];
        let Some(pick) = plan_placement(&c, &offers, &weights) else {
            panic!("expected a placement");
        };
        assert_eq!(pick.node.id, NodeId::new("aaa"));
    }

    #[test]
    fn no_feasible_offer_yields_no_placement() {
        let weights = ScoreWeights::default();
        let c = ResourceClaim {
            min_memory_mb: 1_000_000,
            ..claim()
        };
        let n = node("n");
        let offers = [NodeOffer {
            node: &n,
            signals: PlacementSignals::default(),
        }];
        assert!(plan_placement(&c, &offers, &weights).is_none());
        assert!(plan_placement(&c, &[], &weights).is_none());
    }
}
