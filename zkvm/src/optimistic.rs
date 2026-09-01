use crate::prover::{verify_execution, ZkvmInput};
use state::mpt::Hash;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use types::Request;

/// Challenge window: 7 days in seconds.
pub const CHALLENGE_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Lifecycle state of a committed batch's proof.
#[derive(Debug, Clone, PartialEq)]
pub enum ProofStatus {
    /// Batch is within the 7-day challenge window.  Anyone may submit a fraud
    /// proof by calling [`verify_execution`] and comparing roots.
    InChallengeWindow,
    /// A proof was generated on demand via the fast-finality path, bypassing
    /// the full challenge window (e.g. to service a withdrawal immediately).
    FastFinality { proven_root: Hash },
    /// A ZK proof was generated and verified after a successful challenge.
    Proven { proven_root: Hash },
    /// A challenger proved that `claimed_root` is wrong; `correct_root` is the
    /// root that honest re-execution produces.
    Disputed {
        claimed_root: Hash,
        correct_root: Hash,
    },
}

/// Result returned by [`OptimisticProver::check_challenge_window`].
#[derive(Debug, Clone, PartialEq)]
pub enum ChallengeStatus {
    /// Still inside the 7-day window; `deadline` is the expiry time.
    Open { deadline: SystemTime },
    /// Challenge window has closed with no successful challenge; the batch is
    /// considered final.
    Expired,
    /// Fast-finality proof was generated; the batch is final immediately.
    FastFinality,
    /// Batch was proven correct after a challenge.
    Proven,
    /// Fraud was detected; batch was disputed.
    Disputed,
}

// ---------------------------------------------------------------------------
// Internal bookkeeping
// ---------------------------------------------------------------------------

struct CommittedBatch {
    claimed_root: Hash,
    snapshot: Vec<(Vec<u8>, Vec<u8>)>,
    requests: Vec<Request>,
    challenge_deadline: SystemTime,
    status: ProofStatus,
}

// ---------------------------------------------------------------------------
// OptimisticProver
// ---------------------------------------------------------------------------

/// Manages a queue of committed batches under the optimistic-ZK mechanism.
///
/// Each batch is assumed valid on submission.  A 7-day challenge window allows
/// anyone to prove fraud via [`verify_execution`].  For time-sensitive
/// operations (e.g. withdrawals) the fast-finality path generates a proof
/// on demand.
pub struct OptimisticProver {
    batches: HashMap<u64, CommittedBatch>,
    challenge_window: Duration,
}

impl OptimisticProver {
    pub fn new() -> Self {
        OptimisticProver {
            batches: HashMap::new(),
            challenge_window: Duration::from_secs(CHALLENGE_WINDOW_SECS),
        }
    }

    /// Submit a committed batch.  The batch enters the challenge window
    /// immediately.
    ///
    /// * `root`     – MPT root claimed by the operator after executing `requests`
    ///   against `snapshot`.
    /// * `sequence` – monotonically increasing batch index.
    /// * `snapshot` – pre-batch MPT snapshot (key-value pairs).
    /// * `requests` – ordered list of requests processed in this batch.
    pub fn submit_batch(
        &mut self,
        root: Hash,
        sequence: u64,
        snapshot: Vec<(Vec<u8>, Vec<u8>)>,
        requests: Vec<Request>,
    ) {
        let deadline = SystemTime::now() + self.challenge_window;
        self.batches.insert(
            sequence,
            CommittedBatch {
                claimed_root: root,
                snapshot,
                requests,
                challenge_deadline: deadline,
                status: ProofStatus::InChallengeWindow,
            },
        );
    }

    /// Generate a ZK proof on demand for `sequence` (fast-finality path).
    ///
    /// Re-executes the STF deterministically.  If the computed root matches the
    /// operator's claimed root the batch is marked [`ProofStatus::FastFinality`];
    /// otherwise it is marked [`ProofStatus::Disputed`].
    ///
    /// Returns the proven (re-computed) root.
    pub fn request_fast_finality_proof(&mut self, sequence: u64) -> anyhow::Result<Hash> {
        let batch = self
            .batches
            .get_mut(&sequence)
            .ok_or_else(|| anyhow::anyhow!("batch {} not found", sequence))?;

        let input = ZkvmInput {
            snapshot: batch.snapshot.clone(),
            requests: batch.requests.clone(),
            pre_root: None,
        };
        let output = verify_execution(input)?;
        let proven_root = output.post_root;

        if proven_root == batch.claimed_root {
            batch.status = ProofStatus::FastFinality { proven_root };
        } else {
            let claimed = batch.claimed_root;
            batch.status = ProofStatus::Disputed {
                claimed_root: claimed,
                correct_root: proven_root,
            };
        }

        Ok(proven_root)
    }

    /// Check the challenge-window status of a batch.
    ///
    /// Returns [`ChallengeStatus::Expired`] for unknown sequences (treated as
    /// already finalized / pruned).
    pub fn check_challenge_window(&self, sequence: u64) -> ChallengeStatus {
        let batch = match self.batches.get(&sequence) {
            Some(b) => b,
            None => return ChallengeStatus::Expired,
        };

        match &batch.status {
            ProofStatus::InChallengeWindow => {
                if SystemTime::now() < batch.challenge_deadline {
                    ChallengeStatus::Open {
                        deadline: batch.challenge_deadline,
                    }
                } else {
                    ChallengeStatus::Expired
                }
            }
            ProofStatus::FastFinality { .. } => ChallengeStatus::FastFinality,
            ProofStatus::Proven { .. } => ChallengeStatus::Proven,
            ProofStatus::Disputed { .. } => ChallengeStatus::Disputed,
        }
    }

    /// Return the current [`ProofStatus`] for a batch, or `None` if not found.
    pub fn proof_status(&self, sequence: u64) -> Option<&ProofStatus> {
        self.batches.get(&sequence).map(|b| &b.status)
    }
}

impl Default for OptimisticProver {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prover::{verify_execution, ZkvmInput};
    use types::{AssetId, DepositRequest, Request, UserId};

    fn user(n: u8) -> UserId {
        UserId([n; 20])
    }

    fn deposit(user: &UserId, asset: &str, amount: u64) -> Request {
        Request::Deposit(DepositRequest {
            user: user.clone(),
            asset: AssetId::from_str(asset),
            amount,
            l1_tx_hash: [0u8; 32],
        })
    }

    // ---

    /// A freshly submitted batch should be in the challenge window with a
    /// deadline ~7 days from now.
    #[test]
    fn test_batch_submitted_enters_challenge_window() {
        let mut prover = OptimisticProver::new();
        prover.submit_batch([1u8; 32], 1, vec![], vec![]);

        assert_eq!(
            prover.proof_status(1),
            Some(&ProofStatus::InChallengeWindow)
        );

        match prover.check_challenge_window(1) {
            ChallengeStatus::Open { deadline } => {
                let diff = deadline.duration_since(SystemTime::now()).unwrap();
                let seven_days = Duration::from_secs(CHALLENGE_WINDOW_SECS);
                // Allow a few seconds of slack for test execution time.
                assert!(diff > seven_days - Duration::from_secs(5));
                assert!(diff <= seven_days);
            }
            other => panic!("expected Open, got {:?}", other),
        }
    }

    /// Requesting a fast-finality proof on a correctly-executed batch should
    /// flip status to FastFinality and return the correct root.
    #[test]
    fn test_fast_finality_proof_changes_status() {
        let u = user(1);
        let req = deposit(&u, "USDC", 1_000_000);

        // Compute the expected root by running verify_execution ahead of time.
        let expected = verify_execution(ZkvmInput {
            snapshot: vec![],
            requests: vec![req.clone()],
            pre_root: None,
        })
        .unwrap();
        let expected_root = expected.post_root;

        // Submit the batch with the correct root.
        let mut prover = OptimisticProver::new();
        prover.submit_batch(expected_root, 1, vec![], vec![req]);

        let proven = prover.request_fast_finality_proof(1).unwrap();
        assert_eq!(proven, expected_root);
        assert_eq!(
            prover.proof_status(1),
            Some(&ProofStatus::FastFinality {
                proven_root: expected_root
            })
        );
        assert_eq!(
            prover.check_challenge_window(1),
            ChallengeStatus::FastFinality
        );
    }

    /// Running verify_execution twice on identical inputs must produce the
    /// same post-root (determinism invariant).
    #[test]
    fn test_verify_execution_deterministic_for_valid_inputs() {
        let u = user(2);
        let req = deposit(&u, "ETH", 5_000_000);

        let make_input = || ZkvmInput {
            snapshot: vec![],
            requests: vec![req.clone()],
            pre_root: None,
        };

        let out1 = verify_execution(make_input()).unwrap();
        let out2 = verify_execution(make_input()).unwrap();

        assert_eq!(
            out1.post_root, out2.post_root,
            "same inputs must produce same root"
        );
        assert_ne!(
            out1.post_root, [0u8; 32],
            "post-root must reflect state change"
        );
    }

    /// A tampered pre-batch snapshot (inflated balance) must produce a
    /// different post-root than clean execution, making fraud detectable.
    #[test]
    fn test_tampered_inputs_produce_different_root() {
        let u = user(3);
        let req = deposit(&u, "BTC", 100_000_000);

        // Clean execution.
        let out_clean = verify_execution(ZkvmInput {
            snapshot: vec![],
            requests: vec![req.clone()],
            pre_root: None,
        })
        .unwrap();

        // Build a tampered snapshot that pre-credits the user with a huge balance.
        let tampered_key = state::StateKey::Balance {
            user: u.clone(),
            asset: AssetId::from_str("BTC"),
        }
        .encode();
        let tampered_balance = types::Balance {
            user: u.clone(),
            asset: AssetId::from_str("BTC"),
            available: 999_999_999,
            locked: 0,
        };
        let tampered_val = serde_json::to_vec(&tampered_balance).unwrap();

        let out_tampered = verify_execution(ZkvmInput {
            snapshot: vec![(tampered_key, tampered_val)],
            requests: vec![req],
            pre_root: None,
        })
        .unwrap();

        assert_ne!(
            out_clean.post_root, out_tampered.post_root,
            "tampered pre-state must yield a different post-root"
        );
    }

    /// Submitting a batch with a fraudulent root and then requesting fast-finality
    /// should mark the batch as Disputed.
    #[test]
    fn test_fraudulent_batch_marked_disputed() {
        let u = user(4);
        let req = deposit(&u, "USDC", 500_000);

        let fraudulent_root = [0xdeu8; 32]; // clearly wrong

        let mut prover = OptimisticProver::new();
        prover.submit_batch(fraudulent_root, 1, vec![], vec![req]);

        let proven = prover.request_fast_finality_proof(1).unwrap();
        assert_ne!(proven, fraudulent_root);

        match prover.proof_status(1) {
            Some(ProofStatus::Disputed {
                claimed_root,
                correct_root,
            }) => {
                assert_eq!(*claimed_root, fraudulent_root);
                assert_eq!(*correct_root, proven);
            }
            other => panic!("expected Disputed, got {:?}", other),
        }
        assert_eq!(prover.check_challenge_window(1), ChallengeStatus::Disputed);
    }
}
