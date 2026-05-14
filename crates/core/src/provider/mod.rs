//! Provider / account / region resolver (Phase 7, closes M4).
//!
//! Per [16-provider-resolver.md]. The resolver is the **last** transformation
//! before the exporter. It fills `account_id` / `account_name` / `region` on
//! every [`Resource`](crate::ir::Resource) and `state_account_id` /
//! `state_region` on every [`StateBackend`](crate::ir::StateBackend), by
//! walking the **provider alias → provider block → profile / assume-role /
//! cascade → external profile map** chain documented in spec § 4.
//!
//! ## Public surface
//!
//! - [`ProviderResolver`] / [`DefaultProviderResolver`]: trait + default impl.
//! - [`ProviderContext`]: per-resolution context (profile map, default region, strict-mode flag).
//! - [`ProfileMap`] / [`ProfileEntry`]: the profile → account/region index.
//! - [`SharedProfileMap`]: `ArcSwap<ProfileMap>` alias for atomic re-loads.
//! - [`load_yaml_profile_map`] / [`load_aws_config`] / [`empty_profile_map`]: the three loaders
//!   mirroring spec § 3.
//! - [`extract_account_id`]: ARN → 12-digit account id helper (spec § 4.1).
//! - [`ProviderError`]: thiserror enum.
//!
//! [16-provider-resolver.md]: ../../../specs/16-provider-resolver.md

mod error;
mod profile_map;
mod resolver;

pub use error::ProviderError;
pub use profile_map::{
    AWS_CONFIG_MAX_CHAIN_HOPS, PROFILE_MAP_FILE_MAX_BYTES, ProfileEntry, ProfileMap,
    SharedProfileMap, empty as empty_profile_map, load_aws_config, load_yaml_profile_map, shared,
};
pub use resolver::{
    DefaultProviderResolver, ProviderContext, ProviderResolver, extract_account_id,
};
