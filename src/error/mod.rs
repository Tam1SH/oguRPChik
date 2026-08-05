//! Layered error contexts.
//!
//! There is no `anyhow` and no `thiserror` in this crate's own code (both are
//! being phased out as the rest of the crate migrates — see the refactor
//! plan). Every fallible boundary returns `error_stack::Report<C>`, where `C`
//! is one of the small `#[non_exhaustive]` context enums below, one per layer:
//!
//! `EndpointError` (convention/address parsing) → `TransportError` (bind/
//! connect/accept/io) → `HandshakeError` (version/scheme/attestation) →
//! `RpcError` (public, top-level).
//!
//! Callers match on the layer they care about via [`error_stack::Report`]'s
//! own API: `report.current_context()` for the top context, or
//! `report.downcast_ref::<TransportError>()` / `report.contains::<T>()` to
//! inspect a specific layer without unwrapping the whole chain. That is the
//! documented, supported way to consume errors from this crate — treat it as
//! part of the public API, not an implementation detail.
//!
//! Wire errors are a separate concern: a `capnp::Error` that crosses the
//! network carries only what [`rpc::to_capnp_exception`] explicitly allows
//! through. See that module for why this is a security boundary, not just
//! style.

mod endpoint;
mod handshake;
mod rpc;
mod transport;

pub use endpoint::EndpointError;
pub use handshake::HandshakeError;
pub use rpc::{RpcError, from_capnp_exception, to_capnp_exception};
pub use transport::TransportError;

/// Shorthand used throughout the crate: a result whose error is a
/// [`error_stack::Report`] over context `C`.
pub type Result<T, C> = core::result::Result<T, error_stack::Report<C>>;
