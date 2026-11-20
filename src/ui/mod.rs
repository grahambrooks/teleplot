//! Leptos UI, rendered on the server and hydrated in the browser.

mod api;
mod clock;
mod compare;
mod dashboard;
mod export;
pub mod format;
mod inspector;
mod panels;
pub mod rows;
pub mod series;
mod shapes;
mod shell;
mod theme;
mod timeline;
#[cfg(feature = "hydrate")]
mod uplot;

pub use shell::{App, shell};
