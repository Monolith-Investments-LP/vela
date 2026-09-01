use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TeePlatform {
    AmdSevSnp,
    IntelTdx,
    AwsNitro,
    Placeholder,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AttestationStatus {
    Attested,
    Pending,
    Simulated,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationRequest {
    pub batch_id: u64,
    pub state_root: String,
    pub binary_hash: String,
    pub fill_count: u64,
    pub orders_processed: u64,
    pub timestamp: u64,
    pub operator_address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationRecord {
    pub batch_id: u64,
    pub status: AttestationStatus,
    pub platform: TeePlatform,

    pub attestation_report: Option<Vec<u8>>,
    pub vcek_cert: Option<String>,
    pub measurement: Option<String>,

    pub binary_hash: String,
    pub state_root: String,
    pub batch_id_attested: u64,
    pub fill_count: u64,
    pub operator_address: String,

    pub generated_at: u64,
    pub attestation_time_ms: u64,
    pub attester_version: String,

    pub etherscan_anchor_tx: Option<String>,
    pub verification_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationResult {
    pub record: AttestationRecord,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeeStats {
    pub total_batches: u64,
    pub attested: u64,
    pub simulated: u64,
    pub pending: u64,
    pub failed: u64,
    pub platform: TeePlatform,
    pub binary_hash: String,
    pub platform_status: String,
}

pub trait TeeAttester: Send + Sync {
    fn attest_batch(
        &self,
        request: AttestationRequest,
    ) -> Pin<Box<dyn Future<Output = AttestationResult> + Send + '_>>;

    fn binary_hash(&self) -> String;
    fn platform(&self) -> TeePlatform;
}

pub struct PlaceholderAttester {
    pub binary_hash: String,
}

impl PlaceholderAttester {
    pub fn new() -> Self {
        let hash = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .map(|bytes| {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                hex::encode(hasher.finalize())
            })
            .unwrap_or_else(|| "placeholder-binary-hash-not-computed".to_string());

        Self { binary_hash: hash }
    }
}

impl Default for PlaceholderAttester {
    fn default() -> Self {
        Self::new()
    }
}

impl TeeAttester for PlaceholderAttester {
    fn attest_batch(
        &self,
        request: AttestationRequest,
    ) -> Pin<Box<dyn Future<Output = AttestationResult> + Send + '_>> {
        let binary_hash = self.binary_hash.clone();
        Box::pin(async move {
            AttestationResult {
                record: AttestationRecord {
                    batch_id: request.batch_id,
                    status: AttestationStatus::Simulated,
                    platform: TeePlatform::Placeholder,
                    attestation_report: None,
                    vcek_cert: None,
                    measurement: None,
                    binary_hash,
                    state_root: request.state_root,
                    batch_id_attested: request.batch_id,
                    fill_count: request.fill_count,
                    operator_address: request.operator_address,
                    generated_at: current_time_ms(),
                    attestation_time_ms: 0,
                    attester_version: "placeholder-0.1.0".to_string(),
                    etherscan_anchor_tx: None,
                    verification_note: "TEE hardware attestation requires AMD SEV-SNP \
                        deployment. Real attestation ships post-Stanford AFT Lab (June 2026). \
                        See VEL-T1-04."
                        .to_string(),
                },
                error: None,
            }
        })
    }

    fn binary_hash(&self) -> String {
        self.binary_hash.clone()
    }

    fn platform(&self) -> TeePlatform {
        TeePlatform::Placeholder
    }
}

pub struct AmdSevSnpAttester {
    pub binary_hash: String,
}

impl TeeAttester for AmdSevSnpAttester {
    fn attest_batch(
        &self,
        request: AttestationRequest,
    ) -> Pin<Box<dyn Future<Output = AttestationResult> + Send + '_>> {
        let binary_hash = self.binary_hash.clone();
        Box::pin(async move {
            // TODO VEL-T1-04 Phase 2: AMD SEV-SNP attestation
            // Steps when running inside a confidential VM:
            // 1. Call /dev/sev-guest ioctl to get attestation report
            //    report = sev_guest::get_report(report_data)?
            //    where report_data = sha256(state_root || fill_count || batch_id)
            // 2. Fetch VCEK certificate from AMD KDS:
            //    GET https://kdsintf.amd.com/vcek/v1/Milan/{chip_id}
            // 3. Verify certificate chain: VCEK -> ASK -> ARK
            // 4. Return full attestation record
            //
            // Reference: https://github.com/virtee/sev
            // Reference: https://oasis.net/blog/verifiable-ai-with-tees
            AttestationResult {
                record: AttestationRecord {
                    batch_id: request.batch_id,
                    status: AttestationStatus::Failed,
                    platform: TeePlatform::AmdSevSnp,
                    attestation_report: None,
                    vcek_cert: None,
                    measurement: None,
                    binary_hash,
                    state_root: request.state_root,
                    batch_id_attested: request.batch_id,
                    fill_count: request.fill_count,
                    operator_address: request.operator_address,
                    generated_at: current_time_ms(),
                    attestation_time_ms: 0,
                    attester_version: "amd-sev-snp-stub-0.1.0".to_string(),
                    etherscan_anchor_tx: None,
                    verification_note: "AMD SEV-SNP not yet integrated. Phase 2.".to_string(),
                },
                error: Some(
                    "AMD SEV-SNP integration not yet implemented. See VEL-T1-04.".to_string(),
                ),
            }
        })
    }

    fn binary_hash(&self) -> String {
        self.binary_hash.clone()
    }

    fn platform(&self) -> TeePlatform {
        TeePlatform::AmdSevSnp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_placeholder_attester_returns_simulated() {
        let attester = PlaceholderAttester::new();
        let request = AttestationRequest {
            batch_id: 1,
            state_root: "0xabc".to_string(),
            binary_hash: attester.binary_hash(),
            fill_count: 5,
            orders_processed: 10,
            timestamp: 1_000_000,
            operator_address: "0x1234".to_string(),
        };
        let result = attester.attest_batch(request).await;
        assert!(matches!(result.record.status, AttestationStatus::Simulated));
        assert!(result.error.is_none());
        assert!(matches!(result.record.platform, TeePlatform::Placeholder));
        assert_eq!(result.record.batch_id, 1);
        assert_eq!(result.record.fill_count, 5);
    }

    #[test]
    fn test_attestation_request_serialization_roundtrip() {
        let request = AttestationRequest {
            batch_id: 42,
            state_root: "0xaabb".to_string(),
            binary_hash: "sha256:test".to_string(),
            fill_count: 12,
            orders_processed: 100,
            timestamp: 9_999_999,
            operator_address: "0xoperator".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: AttestationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.batch_id, 42);
        assert_eq!(back.state_root, "0xaabb");
        assert_eq!(back.fill_count, 12);
        assert_eq!(back.orders_processed, 100);
    }
}
