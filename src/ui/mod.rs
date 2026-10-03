//! Leptos UI, rendered on the server and hydrated in the browser.

mod api;
mod clock;
mod dashboard;
mod export;
pub mod format;
mod inspector;
mod panels;
pub mod series;
mod shell;
mod theme;
#[cfg(feature = "hydrate")]
mod uplot;

pub use shell::{App, shell};
