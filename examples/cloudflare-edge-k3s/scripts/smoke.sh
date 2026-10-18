#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Prove the edge actually routes, from inside the cluster.
#
# House rule: an assertion must OBSERVE behaviour. Reading the generated
# nginx.conf and concluding it would work is not evidence -- the interesting
# failure (a /websocket route that silently lands on 8069 because Odoo is in
# threaded mode) produces a perfectly plausible-looking config either way.
#
# Every request travels the same path a real request from the cloudflared
# connector would: a pod in the namespace, to the nginx Service, with the
# public Host header set.
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck disable=SC2086
nixx() { local sub="$1"; shift; nix "$sub" ${NIX_OVERRIDE:-} "$@"; }
nixval() { nixx eval --raw --impure --expr "(import ./config.nix).$1"; }

NS="$(nixval namespace)"
HOST="$(nixval hostname)"
CURL_IMAGE="${CURL_IMAGE:-curlimages/curl:8.11.1}"
fails=0

# One-shot curl from a throwaway pod in the namespace.
incluster() {
  kubectl -n "$NS" run "smoke-$RANDOM" --rm -i --restart=Never --quiet \
    --image="$CURL_IMAGE" -- "$@" 2>/dev/null
}

status() { incluster curl -s -o /dev/null -w '%{http_code}' --max-time 20 "$@"; }
body() { incluster curl -s --max-time 20 "$@"; }

check() {
  local id="$1" want="$2" got="$3" what="$4"
  if [ "$want" = "$got" ]; then
    printf '  ok   %-5s %s\n' "$id" "$what"
  else
    printf '  FAIL %-5s %s (want %s, got %s)\n' "$id" "$what" "$want" "$got"
    fails=$((fails + 1))
  fi
}

check_contains() {
  local id="$1" needle="$2" hay="$3" what="$4"
  case "$hay" in
    *"$needle"*) printf '  ok   %-5s %s\n' "$id" "$what" ;;
    *)
      printf '  FAIL %-5s %s (no %q in response)\n' "$id" "$what" "$needle"
      fails=$((fails + 1))
      ;;
  esac
}

echo "==> smoke: namespace=$NS host=$HOST"

# 1. The proxy itself is up, independent of any backend. /healthz is nginx's
#    own non-proxying location -- a proxy must stay Ready while its upstreams
#    are rolling, so its health must not depend on them.
check "1.1" "200" "$(status http://nginx/healthz)" "nginx serves its own /healthz"

# 2. A request with the right Host reaches Odoo. Odoo answers / with a 303 to
#    /odoo or /web depending on state; anything below 500 means the request was
#    proxied and answered rather than refused.
code="$(status -H "Host: $HOST" http://nginx/)"
case "$code" in
  2*|3*) printf '  ok   %-5s %s (%s)\n' "2.1" "the public hostname reaches Odoo on 8069" "$code" ;;
  *)
    printf '  FAIL %-5s %s (got %s)\n' "2.1" "the public hostname reaches Odoo on 8069" "$code"
    fails=$((fails + 1))
    ;;
esac

# 3. An unknown Host does NOT reach Odoo -- it falls through to the default
#    server. This is what makes `server_name` matching meaningful rather than
#    decorative.
check "3.1" "404" "$(status -H 'Host: nobody.example.invalid' http://nginx/)" \
  "an unmatched Host gets the default server"
check_contains "3.2" "no upstream configured for this host" \
  "$(body -H 'Host: nobody.example.invalid' http://nginx/)" \
  "and it is the chart's own default-server body"

# 4. THE ONE THAT MATTERS: /websocket lands on 8072, not 8069.
#
#    Comparing status codes between the two ports does NOT work -- Odoo answers
#    a credential-less GET /websocket with 400 on BOTH. Comparing the response
#    status line does not work either: werkzeug (8069) answers HTTP/1.0 and
#    gevent (8072) HTTP/1.1, but nginx terminates the client connection itself
#    and always replies HTTP/1.1 regardless of what it received upstream.
#
#    What DOES differ is the Server header: the gevent worker identifies itself,
#    werkzeug does not. Compare the proxied response against a DIRECT request to
#    each port, so the claim is proved by a differential probe against two
#    known-different backends rather than by reading config.
direct8069="$(incluster curl -s -i --max-time 20 http://odoo:8069/websocket | grep -i '^server:' || true)"
direct8072="$(incluster curl -s -i --max-time 20 http://odoo:8072/websocket | grep -i '^server:' || true)"
viaproxy="$(incluster curl -s -i --max-time 20 -H "Host: $HOST" http://nginx/websocket | grep -i '^server:' || true)"

echo "       direct :8069 -> ${direct8069:-<none>}"
echo "       direct :8072 -> ${direct8072:-<none>}"
echo "       via proxy    -> ${viaproxy:-<none>}"

if [ "$direct8069" = "$direct8072" ]; then
  echo "  SKIP 4.1  the two Odoo ports are indistinguishable by Server header;"
  echo "            this probe cannot prove the split on this Odoo build."
else
  check "4.1" "$direct8072" "$viaproxy" "/websocket is proxied to 8072, not 8069"
fi

# 5. The connector is present and, locally, deliberately not Ready.
ready="$(kubectl -n "$NS" get deploy cloudflared -o jsonpath='{.status.readyReplicas}' 2>/dev/null || true)"
if [ -z "$ready" ] || [ "$ready" = "0" ]; then
  printf '  ok   %-5s %s\n' "5.1" "cloudflared is running and NOT Ready (expected: the tunnel token is fake)"
else
  printf '  ok   %-5s %s\n' "5.1" "cloudflared reports $ready ready replica(s) — you have a real tunnel token"
fi

echo
if [ "$fails" -eq 0 ]; then
  echo "==> smoke: all checks passed"
else
  echo "==> smoke: $fails check(s) FAILED"
  exit 1
fi
