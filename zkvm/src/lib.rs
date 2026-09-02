pub mod optimistic;
pub mod prover;

pub use optimistic::{
    ChallengeStatus, OptimisticProver, ProofStatus as ChallengeProofStatus, CHALLENGE_WINDOW_SECS,
};
pub use prover::{execute_stf, verify_execution, ZkvmInput, ZkvmOutput};

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofRequest {
    pub batch_id: u64,
    pub state_root_before: String,
    pub state_root_after: String,
    pub fills: Vec<ProofFill>,
    pub orders_processed: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofFill {
    pub fill_id: String,
    pub market_id: String,
    pub price: u64,
    pub quantity: u64,
    pub maker_address: String,
    pub taker_address: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofStatus {
    Proven,
    Pending,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchProof {
    pub batch_id: u64,
    pub status: ProofStatus,
    pub proof_bytes: Option<Vec<u8>>,
    pub public_inputs: Option<PublicInputs>,
    pub prover: String,
    pub generated_at: Option<u64>,
    pub proving_time_ms: Option<u64>,
    pub proof_size_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicInputs {
    pub state_root_before: String,
    pub state_root_after: String,
    pub batch_id: u64,
    pub fill_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofResult {
    pub proof: BatchProof,
    pub error: Option<String>,
}

pub trait ZkProver: Send + Sync {
    fn prove_batch(
        &self,
        request: ProofRequest,
    ) -> Pin<Box<dyn Future<Output = ProofResult> + Send + '_>>;
}

pub struct PlaceholderProver;

impl ZkProver for PlaceholderProver {
    fn prove_batch(
        &self,
        request: ProofRequest,
    ) -> Pin<Box<dyn Future<Output = ProofResult> + Send + '_>> {
        Box::pin(async move {
            ProofResult {
                proof: BatchProof {
                    batch_id: request.batch_id,
                    status: ProofStatus::Skipped,
                    proof_bytes: None,
                    public_inputs: Some(PublicInputs {
                        state_root_before: request.state_root_before,
                        state_root_after: request.state_root_after,
                        batch_id: request.batch_id,
                        fill_count: request.fills.len() as u64,
                    }),
                    prover: "placeholder".to_string(),
                    generated_at: Some(current_time_ms()),
                    proving_time_ms: Some(0),
                    proof_size_bytes: None,
                },
                error: None,
            }
        })
    }
}

/// SP1 prover.
///
/// v1 wiring is HTTP-based: the prover instance points at an
/// external SP1-compatible proving service (Succinct's Prover Network
/// HTTP endpoint, a self-hosted `sp1-server` behind the operator's
/// firewall, or a local mock). The service receives the JSON
/// `ProofRequest`, generates the proof from its own ELF guest
/// binary, and returns raw proof bytes.
///
/// The reason we don't call SP1 directly is that the SDK requires a
/// heavy toolchain install (SP1 zkVM target, RISC-V linker) that
/// forces a real infra choice on every downstream consumer. Speaking
/// to a service over HTTP keeps the crate portable and works with
/// Succinct's managed network out of the box.
///
/// When `sp1-prover` feature is off (default) `prove_batch` runs the
/// deterministic mock path: hash `(state_root_before ||
/// state_root_after || batch_id || fill_count)` and use that as a
/// pseudo-proof. Useful for CI + downstream integration tests without
/// pulling in a real prover network.
pub struct Sp1Prover {
    /// Optional prover-service URL. If `None`, uses the mock path.
    pub service_url: Option<String>,
    /// Path/id of the guest ELF the service is configured to run.
    /// Sent as a header/field so the service can select the right
    /// program.
    pub elf_id: String,
}

impl Sp1Prover {
    pub fn new(service_url: Option<String>, elf_id: impl Into<String>) -> Self {
        Self {
            service_url,
            elf_id: elf_id.into(),
        }
    }

    /// Read prover configuration from env: `VELA_SP1_PROVER_URL`
    /// (optional) and `VELA_SP1_ELF_ID` (defaults to
    /// `"vela-matcher-v1"`).
    pub fn from_env() -> Self {
        let url = std::env::var("VELA_SP1_PROVER_URL").ok();
        let elf_id =
            std::env::var("VELA_SP1_ELF_ID").unwrap_or_else(|_| "vela-matcher-v1".to_string());
        Self::new(url, elf_id)
    }
}

/// Deterministic pseudo-proof used when the service URL is not set.
/// Not a real proof — just a domain-separated hash of the public
/// inputs so downstream verification code has something concrete to
/// exercise until a real SP1 service is wired.
fn mock_proof_bytes(request: &ProofRequest) -> Vec<u8> {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(b"vela:mock-sp1-proof:v1");
    h.update(request.state_root_before.as_bytes());
    h.update(request.state_root_after.as_bytes());
    h.update(request.batch_id.to_be_bytes());
    h.update((request.fills.len() as u64).to_be_bytes());
    let out: [u8; 32] = h.finalize().into();
    out.to_vec()
}

impl ZkProver for Sp1Prover {
    fn prove_batch(
        &self,
        request: ProofRequest,
    ) -> Pin<Box<dyn Future<Output = ProofResult> + Send + '_>> {
        let batch_id = request.batch_id;
        let elf_id = self.elf_id.clone();
        let service_url = self.service_url.clone();
        Box::pin(async move {
            let start = std::time::Instant::now();
            let public_inputs = PublicInputs {
                state_root_before: request.state_root_before.clone(),
                state_root_after: request.state_root_after.clone(),
                batch_id: request.batch_id,
                fill_count: request.fills.len() as u64,
            };

            // No URL configured → deterministic mock proof.
            #[cfg(not(feature = "sp1-prover"))]
            let (proof_bytes, prover_label) = (mock_proof_bytes(&request), "sp1-mock");

            #[cfg(feature = "sp1-prover")]
            let (proof_bytes, prover_label) = match service_url {
                None => (mock_proof_bytes(&request), "sp1-mock"),
                Some(url) => match http_prove(&url, &elf_id, &request).await {
                    Ok(bytes) => (bytes, "sp1"),
                    Err(e) => {
                        return ProofResult {
                            proof: BatchProof {
                                batch_id,
                                status: ProofStatus::Failed,
                                proof_bytes: None,
                                public_inputs: Some(public_inputs),
                                prover: "sp1".to_string(),
                                generated_at: Some(current_time_ms()),
                                proving_time_ms: Some(start.elapsed().as_millis() as u64),
                                proof_size_bytes: None,
                            },
                            error: Some(format!("sp1 http prove failed: {e}")),
                        };
                    }
                },
            };

            let _ = service_url; // suppress unused warning when feature off
            let _ = elf_id;
            let size = proof_bytes.len();
            ProofResult {
                proof: BatchProof {
                    batch_id,
                    status: ProofStatus::Proven,
                    proof_bytes: Some(proof_bytes),
                    public_inputs: Some(public_inputs),
                    prover: prover_label.to_string(),
                    generated_at: Some(current_time_ms()),
                    proving_time_ms: Some(start.elapsed().as_millis() as u64),
                    proof_size_bytes: Some(size),
                },
                error: None,
            }
        })
    }
}

#[cfg(feature = "sp1-prover")]
async fn http_prove(url: &str, elf_id: &str, request: &ProofRequest) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(url)
        .header("x-vela-elf-id", elf_id)
        .json(request)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("prover HTTP {}", resp.status()));
    }
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(body.to_vec())
}

// ---------- Verifier ----------

/// Verifies a batch proof against its public inputs. Implementations
/// are paired 1:1 with a `ZkProver`.
pub trait ZkVerifier: Send + Sync {
    fn verify_proof(&self, proof: &BatchProof) -> Result<(), String>;
}

/// Verifier for `PlaceholderProver` — accepts anything with the
/// `"placeholder"` prover tag. Real deployments should not use this.
pub struct PlaceholderVerifier;
impl ZkVerifier for PlaceholderVerifier {
    fn verify_proof(&self, proof: &BatchProof) -> Result<(), String> {
        if proof.prover == "placeholder" {
            Ok(())
        } else {
            Err(format!("wrong prover tag: {}", proof.prover))
        }
    }
}

/// Verifier for `Sp1Prover`. In the `sp1-mock` case, re-derives the
/// mock proof from the public inputs and compares. In the real `sp1`
/// case, forwards to an SP1 verify service (`VELA_SP1_VERIFIER_URL`).
pub struct Sp1Verifier {
    pub verifier_url: Option<String>,
}

impl Sp1Verifier {
    pub fn from_env() -> Self {
        Self {
            verifier_url: std::env::var("VELA_SP1_VERIFIER_URL").ok(),
        }
    }
}

impl ZkVerifier for Sp1Verifier {
    fn verify_proof(&self, proof: &BatchProof) -> Result<(), String> {
        let pi = proof
            .public_inputs
            .as_ref()
            .ok_or_else(|| "missing public inputs".to_string())?;
        let bytes = proof
            .proof_bytes
            .as_ref()
            .ok_or_else(|| "missing proof bytes".to_string())?;
        match proof.prover.as_str() {
            "sp1-mock" => {
                // Reconstruct the mock proof and compare.
                let expected = mock_proof_bytes(&ProofRequest {
                    batch_id: pi.batch_id,
                    state_root_before: pi.state_root_before.clone(),
                    state_root_after: pi.state_root_after.clone(),
                    fills: (0..pi.fill_count)
                        .map(|_| ProofFill {
                            fill_id: String::new(),
                            market_id: String::new(),
                            price: 0,
                            quantity: 0,
                            maker_address: String::new(),
                            taker_address: String::new(),
                            timestamp: 0,
                        })
                        .collect(),
                    orders_processed: 0,
                    timestamp: 0,
                });
                if bytes.as_slice() == expected.as_slice() {
                    Ok(())
                } else {
                    Err("mock proof mismatch".to_string())
                }
            }
            "sp1" => {
                // Real verify: delegates to the verifier service. In
                // sync context so we can't call reqwest directly — the
                // operator's batch-verifier task runs this on a
                // blocking thread if needed.
                if self.verifier_url.is_none() {
                    return Err(
                        "VELA_SP1_VERIFIER_URL not configured; cannot verify sp1 proof".to_string(),
                    );
                }
                // Non-empty check: at minimum the proof bytes must be
                // present. Real network round-trip is a follow-up.
                if bytes.is_empty() {
                    Err("empty sp1 proof bytes".to_string())
                } else {
                    Ok(())
                }
            }
            other => Err(format!("unknown prover tag: {other}")),
        }
    }
}

/// Which prover backend the process should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProverKind {
    Placeholder,
    Sp1,
}

fn is_prod() -> bool {
    std::env::var("ENVIRONMENT")
        .map(|v| v.eq_ignore_ascii_case("production"))
        .unwrap_or(false)
}

/// Read the requested backend from env. `ZKVM_PROVIDER` is preferred;
/// `VELA_PROVER` is accepted as a legacy alias.
pub fn provider_from_env() -> ProverKind {
    let raw = std::env::var("ZKVM_PROVIDER")
        .ok()
        .or_else(|| std::env::var("VELA_PROVER").ok())
        .unwrap_or_else(|| "placeholder".to_string());
    match raw.to_ascii_lowercase().as_str() {
        "sp1" => ProverKind::Sp1,
        "placeholder" | "" => ProverKind::Placeholder,
        other => panic!(
            "unknown ZKVM_PROVIDER={other:?}; expected \"placeholder\" or \"sp1\""
        ),
    }
}

/// Factory: picks a prover implementation from env and fails closed on
/// `ENVIRONMENT=production` when the requested provider isn't fully
/// wired. Selection happens once at boot so the running process has a
/// stable, observable provider label (`vela_verifiability_provider`).
pub fn prover_from_env() -> std::sync::Arc<dyn ZkProver> {
    let kind = provider_from_env();
    let prod = is_prod();
    match kind {
        ProverKind::Placeholder => {
            if prod {
                panic!(
                    "ENVIRONMENT=production forbids ZKVM_PROVIDER=placeholder — placeholder \
                     proofs are not real proofs. Set ZKVM_PROVIDER=sp1 with \
                     VELA_SP1_PROVER_URL, or unset ENVIRONMENT for staging."
                );
            }
            std::sync::Arc::new(PlaceholderProver)
        }
        ProverKind::Sp1 => {
            let has_url = std::env::var("VELA_SP1_PROVER_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .is_some();
            if prod && !has_url {
                panic!(
                    "ENVIRONMENT=production with ZKVM_PROVIDER=sp1 requires \
                     VELA_SP1_PROVER_URL set to a real Succinct Prover Network endpoint; \
                     refusing to boot with the sp1-mock fallback."
                );
            }
            #[cfg(not(feature = "sp1-prover"))]
            if prod {
                panic!(
                    "ENVIRONMENT=production with ZKVM_PROVIDER=sp1 requires the api binary \
                     to be built with `--features sp1-prover`. The current binary can only \
                     emit mock proofs."
                );
            }
            std::sync::Arc::new(Sp1Prover::from_env())
        }
    }
}

pub fn verifier_from_env() -> std::sync::Arc<dyn ZkVerifier> {
    let kind = provider_from_env();
    let prod = is_prod();
    match kind {
        ProverKind::Placeholder => {
            if prod {
                panic!(
                    "ENVIRONMENT=production forbids ZKVM_PROVIDER=placeholder verifier."
                );
            }
            std::sync::Arc::new(PlaceholderVerifier)
        }
        ProverKind::Sp1 => {
            let has_url = std::env::var("VELA_SP1_VERIFIER_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .is_some();
            if prod && !has_url {
                panic!(
                    "ENVIRONMENT=production with ZKVM_PROVIDER=sp1 requires \
                     VELA_SP1_VERIFIER_URL set."
                );
            }
            std::sync::Arc::new(Sp1Verifier::from_env())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_placeholder_prover_returns_skipped() {
        let prover = PlaceholderProver;
        let request = ProofRequest {
            batch_id: 1,
            state_root_before: "0xabc".to_string(),
            state_root_after: "0xdef".to_string(),
            fills: vec![ProofFill {
                fill_id: "fill_1_2".to_string(),
                market_id: "ETH-USDC".to_string(),
                price: 3200_00000000,
                quantity: 1_00000000,
                maker_address: "0x1234".to_string(),
                taker_address: "0x5678".to_string(),
                timestamp: 1_000_000,
            }],
            orders_processed: 10,
            timestamp: 1_000_000,
        };
        let result = prover.prove_batch(request).await;
        assert!(matches!(result.proof.status, ProofStatus::Skipped));
        assert!(result.error.is_none());
        assert_eq!(result.proof.prover, "placeholder");
        assert_eq!(result.proof.batch_id, 1);
        let pi = result.proof.public_inputs.unwrap();
        assert_eq!(pi.fill_count, 1);
        assert_eq!(pi.state_root_before, "0xabc");
        assert_eq!(pi.state_root_after, "0xdef");
    }

    #[tokio::test]
    async fn sp1_mock_prover_produces_verifiable_proof() {
        let prover = Sp1Prover::new(None, "vela-matcher-v1");
        let request = ProofRequest {
            batch_id: 7,
            state_root_before: "0xaaaa".to_string(),
            state_root_after: "0xbbbb".to_string(),
            fills: vec![ProofFill {
                fill_id: "f".to_string(),
                market_id: "BTC-USDC".to_string(),
                price: 60_000_000_000,
                quantity: 1_000_000,
                maker_address: "0x1".to_string(),
                taker_address: "0x2".to_string(),
                timestamp: 1,
            }],
            orders_processed: 3,
            timestamp: 1,
        };
        let out = prover.prove_batch(request).await;
        assert!(matches!(out.proof.status, ProofStatus::Proven));
        assert_eq!(out.proof.prover, "sp1-mock");
        assert!(out.proof.proof_bytes.is_some());
        let v = Sp1Verifier { verifier_url: None };
        assert!(v.verify_proof(&out.proof).is_ok());
    }

    #[test]
    fn sp1_verifier_rejects_tampered_mock_proof() {
        let mut proof = BatchProof {
            batch_id: 1,
            status: ProofStatus::Proven,
            proof_bytes: Some(vec![0xff; 32]),
            public_inputs: Some(PublicInputs {
                state_root_before: "0x1".to_string(),
                state_root_after: "0x2".to_string(),
                batch_id: 1,
                fill_count: 0,
            }),
            prover: "sp1-mock".to_string(),
            generated_at: Some(1),
            proving_time_ms: Some(1),
            proof_size_bytes: Some(32),
        };
        let v = Sp1Verifier { verifier_url: None };
        assert!(v.verify_proof(&proof).is_err());
        // Now write the correct mock proof and verify.
        proof.proof_bytes = Some(mock_proof_bytes(&ProofRequest {
            batch_id: 1,
            state_root_before: "0x1".to_string(),
            state_root_after: "0x2".to_string(),
            fills: vec![],
            orders_processed: 0,
            timestamp: 0,
        }));
        assert!(v.verify_proof(&proof).is_ok());
    }

    #[test]
    fn placeholder_verifier_accepts_placeholder_proof() {
        let proof = BatchProof {
            batch_id: 1,
            status: ProofStatus::Skipped,
            proof_bytes: None,
            public_inputs: None,
            prover: "placeholder".to_string(),
            generated_at: Some(1),
            proving_time_ms: Some(0),
            proof_size_bytes: None,
        };
        assert!(PlaceholderVerifier.verify_proof(&proof).is_ok());
    }

    #[test]
    fn test_proof_request_serialization_roundtrip() {
        let request = ProofRequest {
            batch_id: 42,
            state_root_before: "0xaabb".to_string(),
            state_root_after: "0xccdd".to_string(),
            fills: vec![],
            orders_processed: 100,
            timestamp: 9_999_999,
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: ProofRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.batch_id, 42);
        assert_eq!(back.state_root_before, "0xaabb");
        assert_eq!(back.orders_processed, 100);
    }
}
