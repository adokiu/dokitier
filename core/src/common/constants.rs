pub const MANUAL_CONNECTOR_RECONNECT_INTERVAL_MS: u64 = 1000;

pub const OSPF_UPDATE_MY_GLOBAL_FOREIGN_NETWORK_INTERVAL_SEC: u64 = 10;

pub const MAX_DIRECT_CONNS_PER_PEER_IN_FOREIGN_NETWORK: u32 = 3;

pub const DIRECT_CONNECT_TO_PUBLIC_SERVER: bool = true;

// must make it true in future.
pub const HMAC_SECRET_DIGEST: bool = false;

pub const WIN_SERVICE_WORK_DIR_REG_KEY: &str = "SOFTWARE\\EasyTier\\Service\\WorkDir";

// Exclude the release tag from `git describe`: the crate version is already the
// prefix, so a v-tag would duplicate it (e.g. 2.7.0-v2.7.0-3-gabc). We only want
// the commit distance/hash suffix: 2.7.0-3-gc40fee17 or 2.7.0-c40fee17.
pub const EASYTIER_VERSION: &str = git_version::git_version!(
    args = ["--abbrev=8", "--always", "--dirty=~", "--exclude=v*"],
    prefix = concat!(env!("CARGO_PKG_VERSION"), "-"),
    suffix = "",
    fallback = env!("CARGO_PKG_VERSION")
);
