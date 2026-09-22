#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

git init --bare -q "${TMP_DIR}/remote.git"
git clone -q "${TMP_DIR}/remote.git" "${TMP_DIR}/work"
git -C "${TMP_DIR}/work" config user.email test@example.com
git -C "${TMP_DIR}/work" config user.name test
git -C "${TMP_DIR}/work" checkout -q -b main
printf 'one\n' > "${TMP_DIR}/work/file"
git -C "${TMP_DIR}/work" add file
git -C "${TMP_DIR}/work" commit -qm initial
git -C "${TMP_DIR}/work" push -q origin HEAD:main
first_sha="$(git -C "${TMP_DIR}/work" rev-parse HEAD)"

GITHUB_OUTPUT="${TMP_DIR}/valid-output" \
PREPARED_REF="main" \
RELEASE_BRANCH="main" \
  bash -c "cd '${TMP_DIR}/work' && bash '${ROOT_DIR}/scripts/release/resolve-prepared-ref.sh'"

grep -Fqx "source-sha=${first_sha}" "${TMP_DIR}/valid-output"
printf 'PASS: prepared branch resolves to current immutable SHA\n'

printf 'two\n' >> "${TMP_DIR}/work/file"
git -C "${TMP_DIR}/work" add file
git -C "${TMP_DIR}/work" commit -qm newer
git -C "${TMP_DIR}/work" push -q origin HEAD:main

if GITHUB_OUTPUT="${TMP_DIR}/stale-output" \
  PREPARED_REF="${first_sha}" \
  RELEASE_BRANCH="main" \
  bash -c "cd '${TMP_DIR}/work' && bash '${ROOT_DIR}/scripts/release/resolve-prepared-ref.sh'"; then
  printf 'FAIL: stale prepared ref was accepted\n'
  exit 1
fi
printf 'PASS: stale prepared ref is rejected\n'

# Authenticated resolution over smart HTTP. GitHub's Git endpoint accepts
# Basic credentials with an `x-access-token` username and rejects Bearer, so
# the fixture server enforces exactly that contract. A header that GitHub would
# refuse must fail here too, otherwise this test proves nothing about CI.
if ! command -v python3 >/dev/null 2>&1; then
  printf 'SKIP: python3 unavailable; authenticated smart-HTTP resolution not exercised\n'
  exit 0
fi

GIT_HTTP_BACKEND="$(git --exec-path)/git-http-backend"
if [ ! -x "${GIT_HTTP_BACKEND}" ]; then
  printf 'SKIP: git-http-backend unavailable; authenticated smart-HTTP resolution not exercised\n'
  exit 0
fi

TOKEN="fixture-token-$$"
cat > "${TMP_DIR}/server.py" <<'PY'
import base64, http.server, os, subprocess, sys

token = os.environ["FIXTURE_TOKEN"]
project_root = os.environ["FIXTURE_ROOT"]
backend = os.environ["FIXTURE_BACKEND"]
expected = "basic " + base64.b64encode(f"x-access-token:{token}".encode()).decode()

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        header = self.headers.get("Authorization", "")
        if header.lower() != expected.lower():
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="fixture"')
            self.end_headers()
            self.wfile.write(b"invalid credentials\n")
            return
        path, _, query = self.path.partition("?")
        env = dict(os.environ,
                   GIT_PROJECT_ROOT=project_root,
                   GIT_HTTP_EXPORT_ALL="1",
                   PATH_INFO=path,
                   QUERY_STRING=query,
                   REQUEST_METHOD="GET",
                   REMOTE_USER="x-access-token")
        out = subprocess.run([backend], env=env, capture_output=True).stdout
        head, _, body = out.partition(b"\r\n\r\n")
        status = 200
        headers = []
        for line in head.split(b"\r\n"):
            name, _, value = line.decode().partition(":")
            if name.lower() == "status":
                status = int(value.strip().split()[0])
            elif name:
                headers.append((name, value.strip()))
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
PY

FIXTURE_TOKEN="${TOKEN}" FIXTURE_ROOT="${TMP_DIR}" FIXTURE_BACKEND="${GIT_HTTP_BACKEND}" \
  python3 "${TMP_DIR}/server.py" > "${TMP_DIR}/port" 2>"${TMP_DIR}/server.log" &
SERVER_PID=$!
trap 'kill "${SERVER_PID}" 2>/dev/null || true; rm -rf "${TMP_DIR}"' EXIT

for _ in $(seq 1 50); do
  [ -s "${TMP_DIR}/port" ] && break
  sleep 0.1
done
PORT="$(head -n1 "${TMP_DIR}/port")"
[ -n "${PORT}" ] || { printf 'FAIL: fixture HTTP server did not start\n'; cat "${TMP_DIR}/server.log"; exit 1; }

git clone -q "${TMP_DIR}/remote.git" "${TMP_DIR}/http-work"
git -C "${TMP_DIR}/http-work" remote set-url origin "http://127.0.0.1:${PORT}/remote.git"
current_sha="$(git -C "${TMP_DIR}/work" rev-parse HEAD)"

if GIT_TERMINAL_PROMPT=0 GITHUB_OUTPUT="${TMP_DIR}/unauth-output" \
  PREPARED_REF="main" \
  RELEASE_BRANCH="main" \
  bash -c "cd '${TMP_DIR}/http-work' && bash '${ROOT_DIR}/scripts/release/resolve-prepared-ref.sh'" 2>/dev/null; then
  printf 'FAIL: fixture accepted an unauthenticated request, so it cannot tell headers apart\n'
  exit 1
fi
printf 'PASS: fixture rejects unauthenticated resolution\n'

if GIT_TERMINAL_PROMPT=0 git -C "${TMP_DIR}/http-work" \
  -c "http.extraheader=AUTHORIZATION: bearer ${TOKEN}" ls-remote origin refs/heads/main >/dev/null 2>&1; then
  printf 'FAIL: fixture accepted a Bearer header, which GitHub Git rejects\n'
  exit 1
fi
printf 'PASS: fixture rejects Bearer, matching GitHub Git\n'

GIT_TERMINAL_PROMPT=0 GITHUB_OUTPUT="${TMP_DIR}/auth-output" \
SOURCE_TOKEN="${TOKEN}" \
PREPARED_REF="main" \
RELEASE_BRANCH="main" \
  bash -c "cd '${TMP_DIR}/http-work' && bash '${ROOT_DIR}/scripts/release/resolve-prepared-ref.sh'"

grep -Fqx "source-sha=${current_sha}" "${TMP_DIR}/auth-output"
printf 'PASS: SOURCE_TOKEN resolves a prepared ref over authenticated smart HTTP\n'
