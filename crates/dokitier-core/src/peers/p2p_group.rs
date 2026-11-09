//! P2P group policy.
//!
//! Peers in the same (non-empty) group may form automatic direct connections
//! (hole punching).  Different groups are isolated for *p2p* traffic: they may
//! still connect to reachable listeners, because those connections are the
//! relay bridge used to forward traffic between groups.

/// Returns true when a hole-punch attempt to `remote` must be rejected.
pub fn is_cross_group_for(local: Option<&str>, remote: &str) -> bool {
    if remote.is_empty() {
        return false;
    }
    match local.filter(|g| !g.is_empty()) {
        Some(local) => local != remote,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2p_group_policy() {
        assert!(!is_cross_group_for(Some("cn"), "cn"));
        assert!(is_cross_group_for(Some("cn"), "us"));
        assert!(!is_cross_group_for(Some("cn"), ""));
        assert!(!is_cross_group_for(None, "us"));
    }
}
