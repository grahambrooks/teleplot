//! Leptos UI, rendered on the server and hydrated in the browser.

mod api;
mod dashboard;
pub mod format;
mod panels;
pub mod series;
mod shell;
mod theme;
#[cfg(feature = "hydrate")]
mod uplot;

pub use shell::{App, shell};
