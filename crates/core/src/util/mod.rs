//! Internal utilities shared across phases.
//!
//! Members of this module are crate-private — they exist to keep
//! cross-cutting helpers (path safety, byte-counted readers) in one place
//! without leaking implementation details into the public surface.

pub(crate) mod paths;
