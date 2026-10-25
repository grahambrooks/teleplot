# Contributing to Teleplot

Thanks for helping. Bug reports, Kibana exports that don't render, and pull
requests are all welcome. This page covers what's in scope, how to build and
test, and what a pull request needs.

## Before you start

Teleplot is deliberately small. It renders dashboards and only ever reads from
Elasticsearch. Please open an issue before a large change, so we can agree it
fits.

**In scope:** rendering Kibana 8.x/9.x exports and Teleplot YAML faithfully,
new chart types for software-delivery analytics, KQL and ES|QL correctness,
performance, and the CLI and MCP tools.

**Out of scope, by design.** A pull request for one of these will be closed,
however well made:

- ingesting data, or writing to Elasticsearch
- editing dashboards in the UI (dashboards are files)
- authentication or multi-user features (Teleplot serves localhost only)
- alerting and scheduled reports
- TSVB, Timelion, Vega, Maps, and the Kibana 9.4+ Dashboards API format
- OpenSearch
- charts that rank individual engineers. Delivery metrics group by team or
  repository, never by person

[`docs/roadmap.md`](docs/roadmap.md) lists the work that is planned, in order.

## Reporting a bug

Include:

- the Teleplot version (`teleplot --version`) and your OS
- your Elasticsearch version (and Kibana's, if the dashboard is an export)
- for a dashboard that renders wrongly or not at all, the output of
  `teleplot check <export.ndjson>`. If you can, also attach the export, or a
  smaller one that shows the same problem, with any sensitive values removed
- what you expected, and what you saw. A screenshot of the same panel in
  Kibana is the quickest way to show a difference in numbers

**Security issues:** please don't open a public issue. Report them privately
through the repository's **Security** tab (*Report a vulnerability*).

## Setting up

See [Running it after cloning](README.md#running-it-after-cloning). In short:
install [rustup](https://rustup.rs) and
[cargo-leptos](https://github.com/leptos-rs/cargo-leptos). `rust-toolchain.toml`
pins the compiler and the WebAssembly target. Then:

```sh
docker compose -f fixtures/docker-compose.yml up -d es8
cargo run --example seed
make dev        # http://127.0.0.1:3000, reloads on change
```

Use `make dev`, not `cargo run`, to serve the UI. A debug server built with
plain `cargo` can't hydrate the client, and you get a blank page.

Install the git hooks, which run the same checks as CI:

```sh
prek install --hook-type pre-commit --hook-type pre-push
```

## Checks

| Command | What it checks | When |
|---|---|---|
| `make pre-commit` | rustfmt, clippy (server and WASM client, warnings are errors), tests | before every commit |
| `make integration` | Kibana-parity tests against ES 8.19 and 9.5 | changes to queries, KQL, or Kibana conversion |
| `make smoke` | the release binary serves the whole UI from an empty directory | changes to the build or embedded assets |
| `make deny` | advisories, licences and sources of dependencies | dependency changes |

CI runs all four on every pull request.

`make integration` needs both clusters up and seeded:

```sh
docker compose -f fixtures/docker-compose.yml --profile v8 --profile v9 up -d
cargo run --example seed -- --es http://localhost:9208 --end 2026-10-01
cargo run --example seed -- --es http://localhost:9209 --end 2026-10-01
make integration
```

## Tests and fixtures

- **Fixtures are Kibana's own exports, not hand-written.** `make fixtures`
  rebuilds them against dockerised Kibana 8.19 and 9.5 (see
  [`fixtures/README.md`](fixtures/README.md)). Review the screenshots it
  takes before you commit.
- **Kibana parity is measured against those screenshots.** If your change
  alters a number, the parity test should show that Teleplot now matches
  Kibana more closely, not less.
- **Snapshots.** Conversion and query tests use
  [insta](https://insta.rs). Review changes with `cargo insta review`, and
  explain any snapshot change in the pull request.
- **Never drop a panel silently.** A Kibana feature Teleplot can't draw must
  become a labelled placeholder or a visible warning.

## Pull requests

- Keep one change per pull request, with a test that fails without it.
- Write commit subjects in the imperative, describing what changed
  (`Render Lens annotation layers on time axes`).
- Update the docs your change affects: `README.md`, `docs/dashboards.md` for
  the YAML format, and `skills/teleplot/` if agents use the feature.
- Don't add JavaScript dependencies. uPlot is the only one, and it's vendored.
  New charts should reuse existing operations where they can.
- New Rust dependencies need a reason in the pull request, and must pass
  `make deny` (permissive licences only).

`AGENTS.md` has the code map and the build pitfalls. It's written for coding
agents, and it's just as useful for people.

## Licence

Teleplot is licensed under [Apache-2.0](LICENSE). By submitting a contribution,
you agree that it is licensed under the same terms (section 5 of the licence).
