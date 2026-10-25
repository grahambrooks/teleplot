//! YAML in and out, with one set of parser options for every caller.
//!
//! Teleplot YAML is YAML 1.2: only `true` and `false` are booleans.
//! serde-saphyr's defaults follow YAML 1.1 instead, reading `y`, `n`, `yes`,
//! `no`, `on` and `off` as booleans (a column named `n` became `false`) and
//! quoting such strings on output (`"y": 4` in every grid). Errors are one
//! line (`line L column C: …`) because lint and MCP put them in lists and JSON.

use serde::{Serialize, de::DeserializeOwned};

pub use serde_saphyr::Error;

fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        with_snippet: false,
    }
}

/// A value as one YAML document, without a `%YAML` directive.
pub fn to_string<T: Serialize>(value: &T) -> Result<String, serde_saphyr::SerializeError> {
    let options = serde_saphyr::ser_options! {
        yaml_12: true,
        no_lang_directive: true,
    };
    serde_saphyr::to_string_with_options(value, options)
}

/// One YAML document.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_saphyr::from_str_with_options(text, options())
}

/// Every document in a `---`-separated stream.
pub fn from_multiple<T: DeserializeOwned>(text: &str) -> Result<Vec<T>, Error> {
    serde_saphyr::from_multiple_with_options(text, options())
}
