#!/usr/bin/env bash

# Shared bench artifact viewer contract helpers.
#
# Callers that publish generated WordPress site artifacts can source this file
# instead of hand-rolling Playground viewer metadata and URL resolution.

HOMEBOY_BENCH_VIEWER_KIND_WORDPRESS_PLAYGROUND_BLUEPRINT="wordpress-playground-blueprint"
HOMEBOY_BENCH_WORDPRESS_PLAYGROUND_BASE_URL="${HOMEBOY_BENCH_WORDPRESS_PLAYGROUND_BASE_URL:-https://playground.wordpress.net/}"
HOMEBOY_BENCH_WORDPRESS_PLAYGROUND_BLUEPRINT_PARAMETER="blueprint-url"

homeboy_bench_artifact_public_base_url() {
	printf '%s\n' "${HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL:-${HOMEBOY_PUBLIC_ARTIFACT_BASE_URL:-${HOMEBOY_ARTIFACT_PUBLIC_BASE_URL:-}}}"
}

homeboy_bench_artifact_public_url() {
	local artifact_path="$1"
	local public_base_url="${2:-$(homeboy_bench_artifact_public_base_url)}"

	node -e '
const artifactPath = process.argv[1] || "";
const publicBaseUrl = process.argv[2] || "";

if (/^https?:\/\//i.test(artifactPath)) {
  console.log(artifactPath);
  process.exit(0);
}

if (!publicBaseUrl) {
  console.error("public artifact base URL is required to resolve relative artifact paths");
  process.exit(2);
}

const base = publicBaseUrl.endsWith("/") ? publicBaseUrl : `${publicBaseUrl}/`;
const relativePath = artifactPath.replace(/^\.\//, "").replace(/^\/+/g, "");
const encodedPath = relativePath.split("/").map(encodeURIComponent).join("/");
console.log(new URL(encodedPath, base).toString());
' "$artifact_path" "$public_base_url"
}

homeboy_bench_playground_blueprint_viewer_url() {
	local public_artifact_url="$1"
	local playground_base_url="${2:-$HOMEBOY_BENCH_WORDPRESS_PLAYGROUND_BASE_URL}"

	node -e '
const publicArtifactUrl = process.argv[1] || "";
const playgroundBaseUrl = process.argv[2] || "https://playground.wordpress.net/";
const url = new URL(playgroundBaseUrl);
url.searchParams.set("blueprint-url", publicArtifactUrl);
console.log(url.toString());
' "$public_artifact_url" "$playground_base_url"
}

homeboy_bench_playground_blueprint_viewer_contract_json() {
	node -e '
const viewer = {
  kind: "wordpress-playground-blueprint",
  base: process.env.HOMEBOY_BENCH_WORDPRESS_PLAYGROUND_BASE_URL || "https://playground.wordpress.net/",
  query: {
    parameter: "blueprint-url",
    value: { source: "public-artifact-url" },
    encoding: "url"
  }
};
console.log(JSON.stringify(viewer, null, 2));
'
}

homeboy_bench_playground_blueprint_viewer_json() {
	local artifact_path="$1"
	local public_base_url="${2:-$(homeboy_bench_artifact_public_base_url)}"
	local public_artifact_url
	local viewer_url

	public_artifact_url="$(homeboy_bench_artifact_public_url "$artifact_path" "$public_base_url")"
	viewer_url="$(homeboy_bench_playground_blueprint_viewer_url "$public_artifact_url")"

	node -e '
const publicArtifactUrl = process.argv[1];
const viewerUrl = process.argv[2];
const viewer = {
  kind: "wordpress-playground-blueprint",
  base: process.env.HOMEBOY_BENCH_WORDPRESS_PLAYGROUND_BASE_URL || "https://playground.wordpress.net/",
  query: {
    parameter: "blueprint-url",
    value: {
      source: "public-artifact-url",
      url: publicArtifactUrl
    },
    encoding: "url"
  },
  "public-artifact-url": publicArtifactUrl,
  public_artifact_url: publicArtifactUrl,
  url: viewerUrl
};
console.log(JSON.stringify(viewer, null, 2));
' "$public_artifact_url" "$viewer_url"
}

homeboy_bench_require_public_artifact_reachable() {
	local public_artifact_url="$1"

	if curl -fsSIL --max-time 10 "$public_artifact_url" >/dev/null; then
		return 0
	fi

	# Some artifact stores do not implement HEAD; fall back to a tiny GET.
	curl -fsSL --max-time 10 --range 0-0 "$public_artifact_url" >/dev/null
}
