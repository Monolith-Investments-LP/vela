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

pub struct Sp1Prover {
    pub elf_bytes: Vec<u8>,
}

impl ZkProver for Sp1Prover {
    fn prove_batch(
        &self,
        request: ProofRequest,
    ) -> Pin<Box<dyn Future<Output = ProofResult> + Send + '_>> {
        // TODO: integrate SP1 SDK — https://github.com/succinctlabs/sp1
        let batch_id = request.batch_id;
        Box::pin(async move {
            ProofResult {
                proof: BatchProof {
                    batch_id,
                    status: ProofStatus::Failed,
                    proof_bytes: None,
                    public_inputs: None,
                    prover: "sp1".to_string(),
                    generated_at: None,
                    proving_time_ms: None,
                    proof_size_bytes: None,
                },
                error: Some("SP1 integration not yet implemented".to_string()),
            }
        })
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
