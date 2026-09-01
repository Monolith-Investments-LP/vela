use types::{AssetId, MarketId, UserId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StateKey {
    Balance { user: UserId, asset: AssetId },
    Metadata { user: UserId },
    OrderBook { market: MarketId },
    MarketConfig { market: MarketId },
    GlobalSequence,
}

impl StateKey {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            StateKey::Balance { user, asset } => {
                let mut k = b"bal:".to_vec();
                k.extend_from_slice(&user.0);
                k.push(b':');
                k.extend_from_slice(asset.as_str().as_bytes());
                k
            }
            StateKey::Metadata { user } => {
                let mut k = b"meta:".to_vec();
                k.extend_from_slice(&user.0);
                k
            }
            StateKey::OrderBook { market } => {
                let mut k = b"book:".to_vec();
                k.extend_from_slice(market.0.as_bytes());
                k
            }
            StateKey::MarketConfig { market } => {
                let mut k = b"mkt:".to_vec();
                k.extend_from_slice(market.0.as_bytes());
                k
            }
            StateKey::GlobalSequence => b"seq".to_vec(),
        }
    }

    pub fn decode(raw: &[u8]) -> Option<Self> {
        if let Some(rest) = raw.strip_prefix(b"bal:") {
            if rest.len() < 21 {
                return None;
            }
            let user = UserId(rest[..20].try_into().ok()?);
            let s = std::str::from_utf8(&rest[21..]).ok()?;
            let asset = AssetId::from_str(s);
            return Some(StateKey::Balance { user, asset });
        }
        if let Some(rest) = raw.strip_prefix(b"meta:") {
            if rest.len() != 20 {
                return None;
            }
            let user = UserId(rest.try_into().ok()?);
            return Some(StateKey::Metadata { user });
        }
        if let Some(rest) = raw.strip_prefix(b"book:") {
            let market = MarketId(String::from_utf8(rest.to_vec()).ok()?);
            return Some(StateKey::OrderBook { market });
        }
        if let Some(rest) = raw.strip_prefix(b"mkt:") {
            let market = MarketId(String::from_utf8(rest.to_vec()).ok()?);
            return Some(StateKey::MarketConfig { market });
        }
        if raw == b"seq" {
            return Some(StateKey::GlobalSequence);
        }
        None
    }
}
