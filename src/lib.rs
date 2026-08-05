extern crate core;

pub mod auth;
pub mod endpoint;
pub mod error;
pub mod net;
pub mod rpc;

/// Generated from `schema/agent.capnp` by `build.rs` (`capnpc`) — do not
/// edit; edit the schema and rebuild. (Included at the crate root because
/// the generated code refers to itself as `crate::agent_capnp`.)
pub mod agent_capnp {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/agent_capnp.rs"));
}

/// Generated from `schema/plugin.capnp` by `build.rs` (`capnpc`) — do not
/// edit; edit the schema and rebuild.
pub mod plugin_capnp {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/plugin_capnp.rs"));
}
