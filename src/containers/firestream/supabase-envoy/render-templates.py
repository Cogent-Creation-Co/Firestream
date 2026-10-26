"""Turn the upstream Supabase Envoy files into runtime templates.

Usage: render-templates.py <upstream envoy dir> <out dir>

The upstream files hard-code the docker-compose service names and ports.
This replaces each with a ${ENVOY_<SERVICE>_HOST} / ${ENVOY_<SERVICE>_PORT}
placeholder, so compose and the chart choose the addresses at runtime. Every
replacement must match exactly the expected number of times: an upstream
change that moves a value fails the build instead of shipping a stale route.
"""

import re
import sys

src, out = sys.argv[1], sys.argv[2]

# cluster name -> upstream (host, port) in docker/volumes/api/envoy/cds.yaml
CLUSTERS = {
    "auth": ("auth", 9999),
    "rest": ("rest", 3000),
    "realtime": ("realtime-dev.supabase-realtime", 4000),
    "storage": ("storage", 5000),
    "functions": ("functions", 9000),
    "meta": ("meta", 8080),
    "studio": ("studio", 3000),
}


def sub_exact(pattern, repl, text, count, what):
    new, n = re.subn(pattern, repl, text)
    if n != count:
        sys.exit(f"render-templates: expected {count} match(es) for {what}, found {n}")
    return new


with open(f"{src}/cds.yaml") as f:
    cds = f.read()
for name, (host, port) in CLUSTERS.items():
    var = name.upper()
    cds = sub_exact(
        rf"(\n\s+cluster_name: {re.escape(name)}\n(?:.*\n)*?\s+address: ){re.escape(host)}(\n\s+port_value: ){port}\n",
        rf"\g<1>${{ENVOY_{var}_HOST}}\g<2>${{ENVOY_{var}_PORT}}\n",
        cds, 1, f"cluster {name}",
    )

with open(f"{src}/lds.template.yaml") as f:
    lds = f.read()
lds = sub_exact(
    r"(\n\s+address: 0\.0\.0\.0\n\s+port_value: )8000\n",
    r"\g<1>${ENVOY_HTTP_PORT}\n", lds, 1, "listener port")
lds = sub_exact(
    r"host_rewrite_literal: realtime-dev\.supabase-realtime\n",
    "host_rewrite_literal: ${ENVOY_REALTIME_TENANT_HOST}\n", lds, 2, "realtime host rewrite")

with open(f"{src}/envoy.yaml") as f:
    bootstrap = f.read()
bootstrap = sub_exact(r"path: /etc/envoy/cds\.yaml", "path: ${ENVOY_RUN_DIR}/cds.yaml", bootstrap, 1, "cds path")
bootstrap = sub_exact(r"path: /etc/envoy/lds\.yaml", "path: ${ENVOY_RUN_DIR}/lds.yaml", bootstrap, 1, "lds path")

for name, text in (("cds.template.yaml", cds), ("lds.template.yaml", lds), ("envoy.template.yaml", bootstrap)):
    with open(f"{out}/{name}", "w") as f:
        f.write(text)
