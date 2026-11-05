#!/usr/bin/env bash
# (Re)starts the local Ergo used by the tests on 127.0.0.1:6667 (plain) and
# :6697 (TLS, self-signed), with connection limits lifted for test traffic.
set -euo pipefail
cd "$(dirname "$0")/.."

docker rm -f textual-ergo >/dev/null 2>&1 || true
docker run -d --name textual-ergo --restart unless-stopped \
  -p 127.0.0.1:6667:6667 -p 127.0.0.1:6697:6697 \
  -v "$PWD/tests/ergo/ircd.yaml:/ircd/ircd.yaml:ro" \
  ghcr.io/ergochat/ergo:stable >/dev/null

for _ in $(seq 1 40); do
  if docker logs textual-ergo 2>&1 | grep -q "Server running"; then
    exit 0
  fi
  sleep 0.25
done
echo "Ergo did not start; see: docker logs textual-ergo" >&2
exit 1
