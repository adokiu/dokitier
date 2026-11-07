//! P2P group policy.
//!
//! Peers in the same (non-empty) group may form automatic direct or
//! hole-punched connections.  Different groups are intentionally isolated:
//! they can only connect through explicitly configured peer addresses, which
//! enter the peer manager as `Manual`/`Listener` connections and bypass this
//! check.

use std::sync::RwLock;

static LOCAL_GROUP: RwLock<Option<String>> = RwLock::new(None);

/// Set the local node's P2P group (normally once, during instance startup).
pub fn set_group(group: Option<String>) {
    *LOCAL_GROUP.write().unwrap() = group.filter(|g| !g.is_empty());
}

pub fn local_group() -> Option<String> {
    LOCAL_GROUP.read().unwrap().clone()
}

/// Returns true when an automatic P2P connection to `remote` must be rejected.
///
/// An empty remote group means the peer does not implement grouping (or did
/// not advertise one), so it is treated as ungrouped and left untouched for
/// backward compatibility.
pub fn is_cross_group(remote: &str) -> bool {
    is_cross_group_for(local_group().as_deref(), remote)
}

/// Per-network variant: `local` comes from the network identity of the instance.
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
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn p2p_group_policy() {
        let _guard = LOCK.lock().unwrap();

        set_group(Some("cn".to_owned()));
        assert!(!is_cross_group("cn"), "same group must be allowed");
        assert!(is_cross_group("us"), "different group must be blocked");
        assert!(!is_cross_group(""), "legacy peer without group stays allowed");

        set_group(None);
        assert!(!is_cross_group("us"), "ungrouped node accepts any peer");
    }
}
