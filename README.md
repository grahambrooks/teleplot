# Teleplot

Fast, view-only dashboards over Elasticsearch. Teleplot renders Kibana dashboard
exports (`.ndjson`) and its own YAML dashboards, ships as a single binary, and
only ever reads from Elasticsearch. It targets ES/Kibana 8.x and 9.x.

## Running it after cloning

### 1. Install the toolchain

- Rust 1.88 or newer (stable), with the WebAssembly target:

  ```sh
  rustup target add wasm32-unknown-unknown
  ```

- [cargo-leptos](https://github.com/leptos-rs/cargo-leptos), which builds the
  server and the browser client together:

  ```sh
  cargo install cargo-leptos --locked
  ```

- Docker, for a local Elasticsearch. You can skip it if you already have a cluster.

### 2. Start Elasticsearch and load sample data

```sh
docker compose -f fixtures/docker-compose.yml up -d es8
cargo run --example seed
```

This starts Elasticsearch 8.19 on <http://localhost:9208> with security off.
The seeder then writes about 5,400 synthetic delivery documents ending today:
pull requests, deployments, incidents and work items for eight teams. Use
`es9` instead of `es8` for Elasticsearch 9.5 on port 9209, and pass
`--es http://localhost:9209` to the seeder.

### 3. Tell Teleplot where Elasticsearch is

Create `teleplot.yaml` in the repository root:

```yaml
elasticsearch:
  url: http://localhost:9208
```

Without it the UI still lists dashboards, but panels show
"Queries cannot run: teleplot.yaml: No such file or directory".

### 4. Run

```sh
make dev
```

Open <http://127.0.0.1:3000>. The **Delivery Overview** dashboard
(`dashboards/delivery-overview.yaml`) should show DORA metrics over the seeded
data. Pages reload when you edit the code or a dashboard file.

> **Don't use plain `cargo run` to serve the UI in a debug build.** cargo-leptos
> compiles the debug browser client with flags that plain `cargo` doesn't use.
> A server from `cargo run` then renders HTML the client can't hydrate, and you
> get a blank page with an "Unrecoverable hydration error" in the browser
> console. Use `make dev`, or run the server that cargo-leptos built:
>
> ```sh
> cargo leptos build
> target/debug/teleplot serve --dashboards dashboards/
> ```

## Pointing it at your own cluster and dashboards

```yaml
# teleplot.yaml
elasticsearch:
  url: https://es.example.com:9200   # or cloud_id: ...
  api_key: ${ES_API_KEY}              # or username / password
  ca_file: /etc/ssl/corp-ca.pem       # optional extra trusted CA
dashboards: ./dashboards              # Kibana .ndjson exports and Teleplot .yaml
time_zone: Europe/London              # for calendar buckets and date math
```

`${NAME}` is replaced by that environment variable, so secrets stay out of the
file. Drop Kibana exports (Stack Management → Saved Objects → Export) or
Teleplot YAML into the dashboards directory. Teleplot picks up changes
without a restart.

For a quick look without a config file, use `--es` (no auth):

```sh
target/debug/teleplot serve --es http://localhost:9208 --dashboards path/to/exports
```

## Building the single binary

```sh
make release
target/release/teleplot serve --config teleplot.yaml
```

The release binary embeds the UI, so you can copy it anywhere and run it from
any directory. `make smoke` checks exactly that.

## Other commands

| Command | What it does |
|---|---|
| `teleplot check <export.ndjson>…` | Reports which panels of each dashboard render |
| `teleplot query <file> --es URL` | Runs a dashboard's queries and prints each panel as a table |
| `teleplot convert <export.ndjson> --out dir` | Converts Kibana exports to Teleplot YAML |
| `teleplot lint <files>` | Checks dashboards; exits 1 on errors |
| `teleplot mcp` | MCP server over stdio for agents (`serve` also exposes `/mcp`) |

Run any of them with `--help` for options.

## Development

- `make pre-commit`: formatting, clippy (server and WASM client) and tests. CI
  runs the same checks
- `make integration`: Kibana-parity tests. Needs both Elasticsearch
  containers (`--profile v8 --profile v9`) up and seeded
- `make fixtures`: rebuilds the Kibana fixtures. See `fixtures/README.md`

Further docs: `docs/dashboards.md` (YAML dashboard format),
`docs/performance.md`, `docs/mvp-plan.md`.

## License

Apache-2.0
