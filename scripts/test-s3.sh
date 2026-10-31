#!/usr/bin/env bash
# The S3 file store (`--file-store s3://…`) against a local Garage, an
# S3-compatible server, in Docker: started here with a bucket and a key,
# removed afterwards. Arguments go to `cargo test`.
set -euo pipefail

IMAGE=dxflrs/garage:v2.1.0
NAME="notekpr-test-garage-$$"
BUCKET=notekpr-test

hex() { od -An -tx1 -N"$1" /dev/urandom | tr -d ' \n'; }

dir=$(mktemp -d)
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT
cat >"$dir/garage.toml" <<'EOF'
metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "[::]:3901"
rpc_public_addr = "127.0.0.1:3901"

[s3_api]
s3_region = "garage"
api_bind_addr = "[::]:3900"
EOF
chmod 644 "$dir/garage.toml"

docker run -d --name "$NAME" -p 127.0.0.1::3900 \
    -e GARAGE_RPC_SECRET="$(hex 32)" \
    -v "$dir/garage.toml:/etc/garage.toml:ro" "$IMAGE" >/dev/null
g() { docker exec -e RUST_LOG=warn "$NAME" /garage "$@"; }
for _ in $(seq 100); do
    g status >/dev/null 2>&1 && break
    sleep 0.1
done
node=$(g node id -q 2>/dev/null | cut -d@ -f1)
g layout assign -z dc1 -c 1G "$node" >/dev/null
g layout apply --version 1 >/dev/null
key="GK$(hex 12)"
secret=$(hex 32)
g key import --yes -n notekpr "$key" "$secret" >/dev/null
g bucket create "$BUCKET" >/dev/null
g bucket allow --read --write --owner "$BUCKET" --key "$key" >/dev/null
port=$(docker port "$NAME" 3900/tcp | head -1 | sed 's/.*://')

export AWS_ENDPOINT="http://127.0.0.1:$port" AWS_ALLOW_HTTP=true AWS_REGION=garage
export AWS_ACCESS_KEY_ID="$key" AWS_SECRET_ACCESS_KEY="$secret"
export NOTEKPR_TEST_S3_BUCKET="$BUCKET"
cargo test --locked -p notekpr-server --test s3 -- --ignored "$@"
