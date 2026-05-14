//! Stable-within-a-run identifiers for [`Component`] and [`Module`].
//!
//! IDs are [`NonZeroU32`] per CLAUDE.md § Type Design — zero is *not* a valid
//! ID; encoding "missing" as `Option<ComponentId>` makes the missing case
//! cost-free (niche-optimisation collapses `Option<NonZeroU32>` to 4 bytes).
//!
//! IDs are not persisted across parse runs. Do not store them in Parquet.
//!
//! [`Component`]: crate::ir::Component
//! [`Module`]: crate::ir::Module

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
        #[serde(transparent)]
        pub struct $name(NonZeroU32);

        impl $name {
            /// Construct from a raw 1-based index. Returns `None` for zero.
            #[must_use]
            pub const fn new(raw: u32) -> Option<Self> {
                match NonZeroU32::new(raw) {
                    Some(v) => Some(Self(v)),
                    None => None,
                }
            }

            /// Construct from a 0-based index (typical interner output).
            ///
            /// Saturates at `u32::MAX` if `raw` would not fit in a `u32`
            /// (i.e. on 64-bit targets at `raw >= u32::MAX as usize`). One
            /// slot of capacity is sacrificed at the very top; cheap.
            ///
            /// Phase 5 module expansion may push id space toward this
            /// limit. If that becomes likely, replace this constructor
            /// with a fallible one that surfaces `IdSpaceExhausted`.
            #[must_use]
            pub fn from_index(raw: usize) -> Self {
                // `try_from` returns the original on success or `Err` on
                // overflow. On overflow we use `u32::MAX - 1` so the
                // subsequent `+1` lands at `u32::MAX` without wrapping.
                let clamped: u32 = u32::try_from(raw).unwrap_or(u32::MAX - 1);
                let one_based = clamped.saturating_add(1);
                NonZeroU32::new(one_based).map_or(Self(NonZeroU32::MIN), Self)
            }

            /// Raw 1-based id.
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0.get()
            }

            /// 0-based index suitable for `Vec` lookup.
            #[must_use]
            pub const fn index(self) -> usize {
                (self.0.get() - 1) as usize
            }
        }
    };
}

define_id! {
    /// Stable-within-a-run identifier for a [`Component`].
    ///
    /// [`Component`]: crate::ir::Component
    ComponentId
}

define_id! {
    /// Stable-within-a-run identifier for a referenced [`Module`].
    ///
    /// [`Module`]: crate::ir::Module
    ModuleId
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn test_should_round_trip_component_id_from_index() {
        let id = ComponentId::from_index(0);
        assert_eq!(id.get(), 1, "1-based id");
        assert_eq!(id.index(), 0, "0-based index");
    }

    #[test]
    fn test_should_reject_zero_in_component_id_new() {
        assert!(ComponentId::new(0).is_none(), "zero is not a valid id");
        assert_eq!(ComponentId::new(1).map(ComponentId::get), Some(1));
    }

    #[test]
    fn test_should_keep_option_niche_optimisation() {
        // CLAUDE.md § Type Design — using `NonZeroU32` over `u32` keeps
        // `Option<ComponentId>` the same size as `ComponentId` (4 bytes).
        assert_eq!(
            std::mem::size_of::<Option<ComponentId>>(),
            std::mem::size_of::<ComponentId>()
        );
    }

    #[test]
    fn test_should_saturate_component_id_from_giant_index() {
        let id = ComponentId::from_index(usize::MAX);
        // Saturation lands at u32::MAX (capacity − 1 + 1) and the NonZeroU32
        // invariant holds.
        assert_eq!(id.get(), u32::MAX);
    }

    #[test]
    fn test_should_serde_round_trip_component_id() {
        let id = ComponentId::from_index(41);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "42");
        let back: ComponentId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, id);
    }
}
