#!/usr/bin/env bash
# screenshot.sh <kibana-port> <kibana-version> <dashboard-id> <out.png> [height]
# Renders a dashboard to PNG with Kibana's reporting, over the fixture data's
# date range, then scales it to 1600px wide (macOS `sips`, if present).
set -uo pipefail
port=$1 version=$2 id=$3 out=$4 height=${5:-2200}
params="(browserTimezone:UTC,layout:(dimensions:(height:$height,width:1600),id:preserve_layout),locatorParams:(id:DASHBOARD_APP_LOCATOR,params:(dashboardId:$id,timeRange:(from:'2026-04-01T00:00:00.000Z',to:'2026-10-02T00:00:00.000Z'),useHash:!f,viewMode:view)),objectType:dashboard,title:'$id',version:'$version')"

path=""
for _ in $(seq 1 24); do   # Kibana takes up to ~30s to notice a new licence
  response=$(curl -s -X POST "localhost:$port/api/reporting/generate/pngV2" \
    -H 'kbn-xsrf: true' -H 'content-type: application/json' -d "{\"jobParams\":\"$params\"}")
  path=$(echo "$response" | sed -n 's/.*"path":"\([^"]*\)".*/\1/p')
  [ -n "$path" ] && break
  sleep 5
done
[ -z "$path" ] && { echo "report request failed: $response" >&2; exit 1; }

for _ in $(seq 1 60); do
  code=$(curl -s -o "$out" -w '%{http_code}' "localhost:$port$path" -H 'kbn-xsrf: true')
  if [ "$code" = 200 ]; then
    command -v sips >/dev/null && sips -Z 1600 "$out" >/dev/null
    echo "wrote $out"
    exit 0
  fi
  sleep 5
done
echo "report for $id did not complete" >&2
exit 1
