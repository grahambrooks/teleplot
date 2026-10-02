//! rust-embed only tracks files that existed when the server last compiled.
//! Watching the site directory makes a rebuilt client bundle force the server
//! to re-embed it. The WASM build skips this: it writes that directory itself.

fn main() {
    if std::env::var_os("CARGO_FEATURE_SSR").is_some() {
        println!("cargo:rerun-if-changed=target/site");
    } else {
        println!("cargo:rerun-if-changed=build.rs");
    }
}
