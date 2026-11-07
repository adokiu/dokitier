pub use dokitier_proto::api;
#[cfg(feature = "web-client")]
pub use dokitier_proto::web;
pub use dokitier_proto::{
    ALL_DESCRIPTOR_BYTES, acl, common, core_config, error, peer_rpc, rpc_types,
};

#[cfg(feature = "magic-dns")]
pub use dokitier_proto::magic_dns;

#[cfg(test)]
pub mod tests;

pub mod rpc;
