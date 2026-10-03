#!/usr/bin/env bash
# Rebuilds the Kibana fixtures from scratch against real ES + Kibana stacks.
#   fixtures/make-fixtures.sh            # both versions
#   fixtures/make-fixtures.sh v8         # one version
# Needs Docker. Leaves the stacks running; stop them with
#   docker compose -f fixtures/docker-compose.yml --profile v8 --profile v9 down
set -euo pipefail
cd "$(dirname "$0")/.."

COMPOSE="docker compose -f fixtures/docker-compose.yml"
END=2026-10-01   # fixed so the seeded data, and so the fixtures, are reproducible
# Kibana's sample dashboards (eCommerce, Flights, Logs), exported by id so
# nothing else in the Kibana instance leaks into the fixture.
SAMPLE_DASHBOARDS='[{"type":"dashboard","id":"722b74f0-b882-11e8-a6d9-e546fe2bba5f"},{"type":"dashboard","id":"7adfa750-4c81-11e8-b3d7-01146121b73d"},{"type":"dashboard","id":"edf84fe0-e1a0-11e7-b6d5-4dc382ef7f5b"}]'

build() {
  local profile=$1 es=$2 kibana=$3 version=$4 dir=$5
  echo "== $profile: Elasticsearch :$es, Kibana :$kibana ($version)"
  $COMPOSE --profile "$profile" up -d --wait
  mkdir -p "fixtures/kibana/$dir/screenshots"

  cargo run -q --example seed -- --es "http://localhost:$es" --end "$END"

  # Kibana 9.5 restricts this internal API to requests that say they are Kibana's own.
  for set in ecommerce flights logs; do
    curl -sf -X POST "localhost:$kibana/api/sample_data/$set" -H 'kbn-xsrf: true' \
      -H 'x-elastic-internal-origin: kibana' >/dev/null
  done
  curl -sf -X POST "localhost:$kibana/api/saved_objects/_export" \
    -H 'kbn-xsrf: true' -H 'content-type: application/json' \
    -d "{\"objects\":$SAMPLE_DASHBOARDS,\"includeReferencesDeep\":true}" \
    >"fixtures/kibana/$dir/sample-data.ndjson"

  cargo run -q --example kibana_fixtures -- \
    --kibana "http://localhost:$kibana" --out "fixtures/kibana/$dir/delivery.ndjson"

  # Reference screenshots come from Kibana's own reporting, which needs a
  # (local, throwaway) trial licence.
  curl -sf -X POST "localhost:$es/_license/start_trial?acknowledge=true" >/dev/null || true
  # The overview is the longest dashboard: Kibana 9.5 needs the extra height.
  for dashboard in delivery-overview:4400 pr-flow:2200 investment:2200; do
    fixtures/screenshot.sh "$kibana" "$version" "teleplot-${dashboard%:*}" \
      "fixtures/kibana/$dir/screenshots/${dashboard%:*}.png" "${dashboard#*:}"
  done
}

want=${1:-all}
[[ $want == all || $want == v8 ]] && build v8 9208 5608 8.19.22 8.19
[[ $want == all || $want == v9 ]] && build v9 9209 5609 9.5.3 9.5
echo "Fixtures rebuilt. Review the screenshots before committing."
