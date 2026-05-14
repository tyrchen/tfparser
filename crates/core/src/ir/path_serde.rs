//! `serde` adapters for `Arc<Path>` (used in several IR types).

pub mod arc_path {
    //! Adapter module — use with `#[serde(with = "crate::ir::path_serde::arc_path")]`.

    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialize an `Arc<Path>` as a borrowed path string.
    pub fn serialize<S>(path: &Arc<Path>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        path.as_ref().serialize(ser)
    }

    /// Deserialize an `Arc<Path>` via [`PathBuf`].
    pub fn deserialize<'de, D>(de: D) -> Result<Arc<Path>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let pb = PathBuf::deserialize(de)?;
        Ok(Arc::from(pb))
    }
}

pub mod arc_path_opt {
    //! Adapter for `Option<Arc<Path>>`.
    //!
    //! `serde`'s `#[serde(with = "...")]` mandates an `&Option<T>` signature
    //! for the serialize function — that's the convention the derive macro
    //! emits. Clippy's `ref_option` lint is allowed locally for that reason.

    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialize `Option<Arc<Path>>` as `Option<&Path>`.
    #[allow(clippy::ref_option)] // serde `with`-style adapters must take `&Option<T>`
    pub fn serialize<S>(path: &Option<Arc<Path>>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match path {
            Some(p) => p.as_ref().serialize(ser),
            None => ser.serialize_none(),
        }
    }

    /// Deserialize `Option<Arc<Path>>` via `Option<PathBuf>`.
    pub fn deserialize<'de, D>(de: D) -> Result<Option<Arc<Path>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let pb: Option<PathBuf> = Option::deserialize(de)?;
        Ok(pb.map(Arc::from))
    }
}
