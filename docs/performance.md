# Performance

The MVP's targets were:

- the first panel of a 12-panel dashboard over 1M documents loads in under 500 ms;
- the whole dashboard loads in under 2 s with a warm cache.

Both are met with a wide margin.

## Results (2026-10-03)

The machine was an Apple M2 Max (32 GB), with Elasticsearch 8.19.22 in Docker on
the same machine (single node, 1 GB heap). The data was 1,011,958 documents
(`seed --scale 185 --prefix perf-`). The dashboard was
`dashboards/delivery-overview.yaml` plus two copied charts: 12 panels, over the
90 days to 2026-10-02.

### Query engine (`cargo run --release --example bench`, 7 runs)

| Case | Median | Max |
|---|---|---|
| Cold: Elasticsearch caches cleared, fresh Teleplot | 71 ms | 89 ms |
| Warm: Elasticsearch has seen the queries, fresh Teleplot | 49 ms | 53 ms |
| Cached: Teleplot's response cache answers | 4 ms | 5 ms |

The first cold run after starting a process can take up to about 350 ms,
because it also opens connections and fetches field capabilities.

### In the browser (Chrome, release binary, caches cleared and server restarted)

| | |
|---|---|
| Every panel drawn | 182 ms after navigation |
| Data API (`POST /api/dashboards/perf/data`) | 87 ms |
| Client bundle (WASM) | 1.32 MB decoded, **396 KB transferred** (brotli) |

Every panel comes back in one `_msearch`, so the first panel and the last
arrive together. At these timings, streaming panels one by one would not be
worth its complexity.

## What keeps it fast

- **One request per dashboard.** Every panel goes into a single `_msearch` with `size: 0`, `track_total_hits: false` and a `filter_path`.
- **Cache-friendly time ranges.** Ranges ending at `now` round `now` down to 30 seconds. Repeat views and refreshes then send identical searches, which Elasticsearch's shard request cache and Teleplot's own 30-second response cache can both answer.
- **Few buckets.** Kibana's interval rules (50-bucket target, 100-bucket limit) keep date histograms small.
- **Bounded document panels.** Document panels (the cycle-time scatter) fetch only the requested fields, capped at a fixed number of documents.
- **A small client.** uPlot (51 KB) draws the time series. The release build ships brotli and gzip copies of the client, chosen by `Accept-Encoding`, and the client carries no date or time zone library (the browser's own `Date` and `Intl` do that work).

## Reproducing

```sh
docker compose -f fixtures/docker-compose.yml --profile v8 up -d --wait
cargo run --release --example seed -- --es http://localhost:9208 --end 2026-10-01 --scale 185 --prefix perf-
cargo run --release --example bench -- --es http://localhost:9208 --runs 7

# In a browser: write the dashboard, serve it, open /d/perf
cargo run --release --example bench -- --write /tmp/perf/perf.yaml
make release && target/release/teleplot serve --es http://localhost:9208 --dashboards /tmp/perf
```
