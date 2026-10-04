//! Response compatibility: explicit null uses the field's default while other
//! malformed values retain Serde's type errors. Omitted fields use `serde(default)`.

use serde::{Deserialize, Deserializer};

pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
