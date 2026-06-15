use std::fs;
use std::process::Command;

pub(super) const TRACE_FIXTURE_EXTENSION_ID: &str = "trace-fixture";

pub(super) fn write_trace_extension(home: &tempfile::TempDir) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join(TRACE_FIXTURE_EXTENSION_ID);
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join(format!("{TRACE_FIXTURE_EXTENSION_ID}.json")),
        r#"{
                "name": "Fixture Trace",
                "version": "0.0.0",
                "trace": { "extension_script": "trace-runner.sh" }
            }"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("trace-runner.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
scenario_ids=""
old_ifs="$IFS"
IFS=":"
for workload in ${HOMEBOY_TRACE_EXTRA_WORKLOADS:-}; do
  name="$(basename "$workload")"
  name="${name%%.trace.*}"
  name="${name%.*}"
  if [ -n "$scenario_ids" ]; then
    scenario_ids="$scenario_ids $name"
  else
    scenario_ids="$name"
  fi
done
IFS="$old_ifs"

if [ "$HOMEBOY_TRACE_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenarios":[
JSON
  comma=""
  printf '%s\n' "${HOMEBOY_TRACE_EXTRA_WORKLOADS:-}" | tr ':' '\n' | while IFS= read -r workload; do
    [ -n "$workload" ] || continue
    name="$(basename "$workload")"
    name="${name%%.trace.*}"
    name="${name%.*}"
    printf '%s{"id":"%s","source":"%s"}' "$comma" "$name" "$workload" >> "$HOMEBOY_TRACE_RESULTS_FILE"
    comma=","
  done
  printf ']}' >> "$HOMEBOY_TRACE_RESULTS_FILE"
  exit 0
fi

case " $scenario_ids " in
  *" $HOMEBOY_TRACE_SCENARIO "*) ;;
  *) printf 'unknown scenario %s\n' "$HOMEBOY_TRACE_SCENARIO" >&2; exit 3 ;;
esac

cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"pass","timeline":[{"t_ms":0,"source":"runner","event":"boot"},{"t_ms":125,"source":"runner","event":"ready"}],"span_results":[{"id":"boot_to_ready","from":"runner.boot","to":"runner.ready","status":"ok","duration_ms":125,"from_t_ms":0,"to_t_ms":125}],"assertions":[],"artifacts":[{"label":"trace log","path":"artifacts/trace-log.txt"}]}
JSON
mkdir -p "$HOMEBOY_TRACE_ARTIFACT_DIR"
printf 'trace log\n' > "$HOMEBOY_TRACE_ARTIFACT_DIR/trace-log.txt"
"#,
    )
    .expect("write trace script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod script");
    }
}

pub(super) fn write_trace_port_env_extension(home: &tempfile::TempDir) {
    write_custom_trace_extension(
        home,
        r#"#!/bin/sh
set -eu
if [ "$HOMEBOY_TRACE_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenarios":[{"id":"studio-app-create-site","source":"fixture"}]}
JSON
  exit 0
fi

test -n "${HOMEBOY_INVOCATION_PORT_BASE:-}"
test -n "${HOMEBOY_INVOCATION_PORT_MAX:-}"
expected_max=$((HOMEBOY_INVOCATION_PORT_BASE + 2))
test "$HOMEBOY_INVOCATION_PORT_MAX" = "$expected_max"

mkdir -p "$HOMEBOY_TRACE_ARTIFACT_DIR"
printf 'base=%s\nmax=%s\n' "$HOMEBOY_INVOCATION_PORT_BASE" "$HOMEBOY_INVOCATION_PORT_MAX" > "$HOMEBOY_TRACE_ARTIFACT_DIR/invocation-ports.txt"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"pass","timeline":[],"assertions":[{"id":"invocation-ports","status":"pass","message":"port range env was present"}],"artifacts":[{"label":"Invocation ports","path":"artifacts/invocation-ports.txt"}]}
JSON
"#,
    );
}

pub(super) fn write_nested_trace_artifact_extension(home: &tempfile::TempDir) {
    write_custom_trace_extension(
        home,
        r#"#!/bin/sh
set -eu
if [ "$HOMEBOY_TRACE_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenarios":[{"id":"studio-app-create-site","source":"fixture"}]}
JSON
  exit 0
fi
mkdir -p "$HOMEBOY_TRACE_ARTIFACT_DIR/provider-artifacts/runtime-fixture/files/browser"
printf '{"url":"https://example.test"}\n' > "$HOMEBOY_TRACE_ARTIFACT_DIR/provider-artifacts/runtime-fixture/files/browser/network.jsonl"
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"pass","timeline":[],"assertions":[],"artifacts":[{"label":"Browser network log","path":"provider-artifacts/runtime-fixture/files/browser/network.jsonl"}]}
JSON
"#,
    );
}

pub(super) fn write_missing_trace_artifact_extension(home: &tempfile::TempDir) {
    write_custom_trace_extension(
        home,
        r#"#!/bin/sh
set -eu
if [ "$HOMEBOY_TRACE_LIST_ONLY" = "1" ]; then
  cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenarios":[{"id":"studio-app-create-site","source":"fixture"}]}
JSON
  exit 0
fi
cat > "$HOMEBOY_TRACE_RESULTS_FILE" <<JSON
{"component_id":"$HOMEBOY_COMPONENT_ID","scenario_id":"$HOMEBOY_TRACE_SCENARIO","status":"pass","timeline":[],"assertions":[],"artifacts":[{"label":"Missing browser log","path":"provider-artifacts/runtime-fixture/files/browser/network.jsonl"}]}
JSON
"#,
    );
}

fn write_custom_trace_extension(home: &tempfile::TempDir, script: &str) {
    let extension_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("extensions")
        .join(TRACE_FIXTURE_EXTENSION_ID);
    fs::create_dir_all(&extension_dir).expect("mkdir extension");
    fs::write(
        extension_dir.join(format!("{TRACE_FIXTURE_EXTENSION_ID}.json")),
        r#"{
                "name": "Fixture Trace",
                "version": "0.0.0",
                "trace": { "extension_script": "trace-runner.sh" }
            }"#,
    )
    .expect("write extension manifest");

    let script_path = extension_dir.join("trace-runner.sh");
    fs::write(&script_path, script).expect("write trace script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod script");
    }
}

pub(super) fn init_overlay_component(path: &std::path::Path) {
    fs::write(path.join("scenario.txt"), "base\n").expect("write scenario");
    run_git(path, &["init"]);
    run_git(path, &["add", "scenario.txt"]);
    run_git(
        path,
        &[
            "-c",
            "user.name=Homeboy Test",
            "-c",
            "user.email=homeboy@example.test",
            "commit",
            "-m",
            "initial",
        ],
    );
}

fn run_git(path: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn write_trace_rig(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{ "path": "${{components.{component_id}.path}}/studio-app-create-site.trace.mjs" }},
                        {{ "path": "${{components.{component_id}.path}}/studio-list-sites.trace.mjs" }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

pub(super) fn write_trace_rig_with_port_range(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{
                            "path": "${{components.{component_id}.path}}/studio-app-create-site.trace.mjs",
                            "port_range_size": 3
                        }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

pub(super) fn write_trace_rig_with_phase_preset(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{
                            "path": "${{components.{component_id}.path}}/studio-app-create-site.trace.mjs",
                            "check_groups": [],
                            "trace_default_phase_preset": "startup",
                            "trace_phase_presets": {{
                                "startup": ["boot:runner.boot", "ready:runner.ready"]
                            }}
                        }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

pub(super) fn write_trace_rig_with_span_metadata(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{
                            "path": "${{components.{component_id}.path}}/studio-app-create-site.trace.mjs",
                            "check_groups": [],
                            "trace_default_phase_preset": "startup",
                            "trace_phase_presets": {{
                                "startup": ["boot:runner.boot", "ready:runner.ready"]
                            }},
                            "trace_span_metadata": {{
                                "phase.boot_to_ready": {{
                                    "critical": true,
                                    "blocking": true,
                                    "cacheable": true,
                                    "prewarmable": true,
                                    "blocks": "first_site_render",
                                    "category": "wordpress_boot"
                                }},
                                "missing_span": {{ "deferrable": true }}
                            }}
                        }}
                    ] }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}

pub(super) fn write_trace_rig_with_variant(
    home: &tempfile::TempDir,
    package_path: &std::path::Path,
    rig_id: &str,
    component_id: &str,
    path: &std::path::Path,
) {
    let sources_dir = home
        .path()
        .join(".config")
        .join("homeboy")
        .join("rig-sources");
    fs::create_dir_all(&sources_dir).expect("mkdir rig sources");
    fs::write(
        sources_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                "source": "{}",
                "package_path": "{}",
                "rig_path": "{}/rig.json",
                "linked": true,
                "source_revision": null
            }}"#,
            package_path.display(),
            package_path.display(),
            package_path.display()
        ),
    )
    .expect("write rig source metadata");

    let overlay_dir = package_path.join("overlays");
    fs::create_dir_all(&overlay_dir).expect("mkdir overlays");
    fs::write(
        overlay_dir.join("fresh-install-mode.patch"),
        r#"diff --git a/scenario.txt b/scenario.txt
--- a/scenario.txt
+++ b/scenario.txt
@@ -1 +1 @@
-base
+overlay
"#,
    )
    .expect("write variant overlay");
    let rig_dir = home.path().join(".config").join("homeboy").join("rigs");
    fs::create_dir_all(&rig_dir).expect("mkdir rigs");
    fs::write(
        rig_dir.join(format!("{}.json", rig_id)),
        format!(
            r#"{{
                    "components": {{
                        "{component_id}": {{ "path": "{}" }}
                    }},
                    "trace_workloads": {{ "{TRACE_FIXTURE_EXTENSION_ID}": [
                        {{ "path": "${{components.{component_id}.path}}/studio-app-create-site.trace.mjs" }}
                    ] }},
                    "trace_variants": {{
                        "fresh-install-mode": {{
                            "component": "{component_id}",
                            "overlay": "overlays/fresh-install-mode.patch"
                        }}
                    }}
                }}"#,
            path.display()
        ),
    )
    .expect("write rig");
}
