use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    StaticSegment,
    components::{A, Route, Router, Routes},
    path,
};

use super::dashboard::{DashboardList, DashboardPage};

/// The HTML document the server renders around [`App`].
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en" data-theme="dark">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <link rel="stylesheet" href="/vendor/uplot/uPlot.min.css"/>
                <script src="/vendor/uplot/uPlot.iife.min.js"></script>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    let dark = RwSignal::new(true);
    provide_context(dark);

    view! {
        <Stylesheet id="teleplot" href="/pkg/teleplot.css"/>
        <Title text="Teleplot"/>
        <Router>
            <TopBar dark/>
            <main class="canvas">
                <Routes fallback=|| view! { <EmptyState title="Page not found"/> }>
                    <Route path=StaticSegment("") view=DashboardList/>
                    <Route path=path!("/d/:id") view=DashboardPage/>
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn TopBar(dark: RwSignal<bool>) -> impl IntoView {
    // The theme lives on <html> so print and export styles can key off it too.
    Effect::new(move |_| {
        if let Some(root) = document().document_element() {
            let theme = if dark.get() { "dark" } else { "light" };
            let _ = root.set_attribute("data-theme", theme);
        }
    });

    view! {
        <header class="topbar">
            <A href="/"><span class="brand">"Teleplot"</span></A>
            <span class="spacer"></span>
            <button
                class="btn"
                aria-label="Toggle theme"
                on:click=move |_| dark.update(|d| *d = !*d)
            >
                {move || if dark.get() { "Light" } else { "Dark" }}
            </button>
        </header>
    }
}

#[component]
fn EmptyState(title: &'static str) -> impl IntoView {
    view! {
        <section class="empty">
            <h1>{title}</h1>
            <p><A href="/">"All dashboards"</A></p>
        </section>
    }
}
