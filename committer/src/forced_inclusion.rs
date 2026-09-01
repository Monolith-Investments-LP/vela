use std::collections::VecDeque;
use std::time::{Duration, SystemTime};
use types::{Request, UserId};

// ---------------------------------------------------------------------------
// ForcedEntry
// ---------------------------------------------------------------------------

/// A single pending entry in the delayed inbox.
#[derive(Debug, Clone)]
pub struct ForcedEntry {
    /// The transaction the user wants force-included.
    pub request: Request,
    /// L1 address (user) that submitted the transaction.
    pub from: UserId,
    /// Wall-clock time when the entry was submitted to the delayed inbox.
    /// In production this would be the L1 block timestamp.
    pub submitted_at: SystemTime,
}

// ---------------------------------------------------------------------------
// DelayedInbox
// ---------------------------------------------------------------------------

/// L1 delayed inbox — the mechanism that makes Vela censorship-resistant.
///
/// If the Vela sequencer refuses to include a user's transaction, the user
/// submits it directly to the `DelayedInbox` smart contract on L1.  After
/// `timeout` elapses without the sequencer including it, anyone can advance
/// the inbox pointer and force the transaction into the next committed batch.
/// This makes censorship-resistance equivalent to an Ethereum L2 (the same
/// pattern used by Arbitrum's delayed inbox).
///
/// # Flow
///
/// 1. User calls `submit_forced(request, from)` on L1 (simulated here in
///    software).
/// 2. The committer calls `drain_eligible(timeout)` before every batch.
///    Transactions older than `timeout` are returned and **prepended** to the
///    pending batch so they are the first to execute.
/// 3. If the sequencer has already included the transaction normally (via the
///    fast path), it will not appear in the delayed inbox and nothing happens.
pub struct DelayedInbox {
    queue: VecDeque<ForcedEntry>,
}

impl DelayedInbox {
    pub fn new() -> Self {
        DelayedInbox {
            queue: VecDeque::new(),
        }
    }

    /// Add a transaction to the delayed inbox using the current wall-clock time
    /// as the submission timestamp.
    pub fn submit_forced(&mut self, request: Request, from: UserId) {
        self.queue.push_back(ForcedEntry {
            request,
            from,
            submitted_at: SystemTime::now(),
        });
    }

    /// Add a transaction with an explicit submission timestamp.
    ///
    /// Primarily used for testing (to simulate entries that were submitted in
    /// the past and are therefore already eligible).
    pub fn submit_forced_at(&mut self, request: Request, from: UserId, submitted_at: SystemTime) {
        self.queue.push_back(ForcedEntry {
            request,
            from,
            submitted_at,
        });
    }

    /// Push a pre-built `ForcedEntry` (used by the committer when draining the
    /// channel from `CommitterHandle::force_include`).
    pub(crate) fn push(&mut self, entry: ForcedEntry) {
        self.queue.push_back(entry);
    }

    /// Drain all entries that have been waiting longer than `timeout`.
    ///
    /// Entries that have not yet aged past `timeout` remain in the queue so
    /// they will be re-evaluated on the next batch interval.
    ///
    /// Returns the eligible [`Request`]s, ready to be prepended to the next
    /// batch.
    pub fn drain_eligible(&mut self, timeout: Duration) -> Vec<Request> {
        let now = SystemTime::now();
        let mut eligible = Vec::new();
        let mut remaining = VecDeque::new();

        while let Some(entry) = self.queue.pop_front() {
            let age = now
                .duration_since(entry.submitted_at)
                .unwrap_or(Duration::ZERO);
            if age >= timeout {
                eligible.push(entry.request);
            } else {
                remaining.push_back(entry);
            }
        }

        self.queue = remaining;
        eligible
    }

    /// Number of transactions currently sitting in the inbox (eligible or not).
    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }
}

impl Default for DelayedInbox {
    fn default() -> Self {
        Self::new()
    }
}
