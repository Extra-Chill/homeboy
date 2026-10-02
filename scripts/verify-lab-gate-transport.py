"""Run the complete native gate transport against an owned loopback SSH runner.

Both Homeboy processes use the supplied candidate binary. Keys, configuration,
daemon, SSH client state and workspaces are private fixtures, never operator state.
"""
import getpass
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
evidence = Path(sys.argv[2]).resolve()
evidence.mkdir(parents=True, exist_ok=True)
rust_source = str(Path(sys.argv[3]).absolute())
original_path = os.environ["PATH"]
original_home = Path(os.environ["HOME"])


def environment(root):
    env = {name: value for name, value in os.environ.items()
           if not name.startswith(("HOMEBOY_", "XDG_", "WORKFLOW_"))}
    # Existing build identity contract for a hash-verified source snapshot whose
    # transfer excludes .git. Keep caller and receiver builds on that provenance.
    for name in ("HOMEBOY_PRODUCT_GIT_COMMIT", "HOMEBOY_PRODUCT_GIT_DIRTY"):
        if name in os.environ:
            env[name] = os.environ[name]
    home = root / "home"
    home.mkdir(parents=True, exist_ok=True)
    env.update(HOME=str(home), HOMEBOY_CONFIG_ROOT=str(root / "config"),
               HOMEBOY_DATA_DIR=str(root / "data"), HOMEBOY_ARTIFACT_ROOT=str(root / "artifacts"),
               HOMEBOY_RUNTIME_TMPDIR=str(root / "runtime"), TMPDIR=str(root / "tmp"),
               CARGO_HOME=str(original_home / ".cargo"), RUSTUP_HOME=str(original_home / ".rustup"))
    for name in ("HOMEBOY_CONFIG_ROOT", "HOMEBOY_DATA_DIR", "HOMEBOY_ARTIFACT_ROOT",
                 "HOMEBOY_RUNTIME_TMPDIR", "TMPDIR"):
        Path(env[name]).mkdir(parents=True, exist_ok=True)
    return env


with tempfile.TemporaryDirectory(prefix="homeboy-native-gate-transport-") as directory:
    root = Path(directory).resolve()
    controller = environment(root / "controller")
    receiver = environment(root / "receiver")
    receiver_binary = root / "receiver/candidate-runtime/homeboy"
    receiver_binary.parent.mkdir()
    shutil.copy2(binary, receiver_binary)
    workspaces = root / "receiver/workspaces"
    workspaces.mkdir()
    client_bin = root / "client-bin"
    client_bin.mkdir()
    keys = root / "keys"
    keys.mkdir(mode=0o700)
    for name in ("host", "client"):
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(keys / name)], check=True)
    authorized = keys / "authorized_keys"
    authorized.write_bytes((keys / "client.pub").read_bytes())
    authorized.chmod(0o600)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    known_hosts = keys / "known_hosts"
    host_public = (keys / "host.pub").read_text().split()
    known_hosts.write_text(f"[127.0.0.1]:{port} {host_public[0]} {host_public[1]}\n")
    client_config = keys / "ssh_config"
    client_config.write_text(f"Host native-gate-loopback\n HostName 127.0.0.1\n Port {port}\n User {getpass.getuser()}\n IdentityFile {keys / 'client'}\n IdentitiesOnly yes\n UserKnownHostsFile {known_hosts}\n StrictHostKeyChecking yes\n")
    for program in ("ssh", "scp"):
        wrapper = client_bin / program
        wrapper.write_text("#!/bin/sh\nexec /usr/bin/" + program + " -F " + shlex.quote(str(client_config)) + " -o " +
                           shlex.quote("UserKnownHostsFile=" + str(known_hosts)) +
                           " -o GlobalKnownHostsFile=/dev/null \"$@\"\n")
        wrapper.chmod(0o700)
    ssh_config = keys / "sshd_config"
    ssh_config.write_text(f"Port {port}\nListenAddress 127.0.0.1\nHostKey {keys / 'host'}\n"
                          f"PidFile {keys / 'sshd.pid'}\nAuthorizedKeysFile {authorized}\n"
                          "PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n"
                          "StrictModes yes\nAllowTcpForwarding yes\nSubsystem sftp internal-sftp\n"
                          f"AllowUsers {getpass.getuser()}\n")
    ssh_log = open(evidence / "sshd.stderr.txt", "w")
    sshd = subprocess.Popen(["/usr/sbin/sshd", "-D", "-e", "-f", str(ssh_config)], stderr=ssh_log)
    controller["PATH"] = str(client_bin) + ":" + original_path
    controller["HOMEBOY_CONTROLLER_ID"] = "native-15359-private-controller"
    controller["HOMEBOY_READONLY_PROBE_TIMEOUT_SECONDS"] = "30"
    receiver_bin = root / "receiver/bin"
    receiver_bin.mkdir()
    probe = receiver_bin / "native-gate-tool"
    probe_marker = root / "receiver/tool-probed"
    probe.write_text("#!/bin/sh\ntest \"$1\" = --version || exit 3\nprintf 'probed\\n' >> " +
                     shlex.quote(str(probe_marker)) + "\nprintf 'native gate tool 1\\n'\n")
    probe.chmod(0o700)
    receiver["PATH"] = str(receiver_bin) + ":" + original_path
    receiver["HOMEBOY_CONTROLLER_ID"] = controller["HOMEBOY_CONTROLLER_ID"]

    def cli(*args, env=controller):
        output = subprocess.run([binary, *args], env=env, capture_output=True, text=True)
        if output.returncode:
            raise RuntimeError(f"fixture command failed: {args}\n{output.stdout}\n{output.stderr}")
        return output

    connected = False
    try:
        for _ in range(100):
            if sshd.poll() is not None:
                raise RuntimeError("private sshd failed: " + (evidence / "sshd.stderr.txt").read_text())
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=.1):
                    break
            except OSError:
                time.sleep(.05)
        server = root / "server.json"
        server.write_text(json.dumps({"id":"native-gate-lab", "host":"native-gate-loopback", "user":getpass.getuser(),
            "port":port, "identity_file":str(keys / "client"), "env":{name:value for name,value in receiver.items()
                if name in ("HOME", "PATH", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR") or name.startswith("HOMEBOY_")},
            "runner":{"workspace_root":str(workspaces), "homeboy_path":str(receiver_binary), "daemon":True,
                      "concurrency_limit":2}}))
        cli("server", "create", "--json", "@" + str(server))
        remote_env = {name:value for name,value in receiver.items()
                      if name in ("HOME", "PATH", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR") or name.startswith("HOMEBOY_")}
        probe_command = "env " + " ".join(shlex.quote(name + "=" + value) for name,value in remote_env.items()) + " " + shlex.quote(str(receiver_binary)) + " self identity"
        probe_result = subprocess.run([str(client_bin / "ssh"), "native-gate-loopback", probe_command],
                                      env=controller, capture_output=True, text=True)
        (evidence / "receiver-identity.stdout.json").write_text(probe_result.stdout)
        (evidence / "receiver-identity.stderr.txt").write_text(probe_result.stderr)
        assert probe_result.returncode == 0, probe_result.stdout + probe_result.stderr
        connection = subprocess.run([binary, "runner", "connect", "native-gate-lab"], env=controller, capture_output=True, text=True)
        (evidence / "connect.json").write_text(connection.stdout)
        (evidence / "connect.stderr.txt").write_text(connection.stderr)
        if connection.returncode:
            failure = json.loads(connection.stdout)["data"]["connection"].get("failure_evidence", {})
            start = failure.get("remote_start_command")
            if start:
                prefix = "export " + " ".join(shlex.quote(name + "=" + value) for name,value in remote_env.items()) + "; "
                status_command = start.split(" daemon ensure-running", 1)[0] + " daemon status"
                status_output = subprocess.run([str(client_bin / "ssh"), "native-gate-loopback", prefix + status_command],
                                              env=controller, capture_output=True, text=True, timeout=90)
                (evidence / "bootstrap-before-status.stdout.json").write_text(status_output.stdout)
                (evidence / "bootstrap-before-status.stderr.txt").write_text(status_output.stderr)
                bootstrap = subprocess.run([str(client_bin / "ssh"), "native-gate-loopback", prefix + start],
                                           env=controller, capture_output=True, text=True, timeout=90)
                (evidence / "bootstrap.stdout.json").write_text(bootstrap.stdout)
                (evidence / "bootstrap.stderr.txt").write_text(bootstrap.stderr)
                assert bootstrap.returncode == 0, bootstrap.stdout + bootstrap.stderr
                # The fixture service is now running through its real native
                # owner. Re-observe admission; do not inject a session or status.
                connection = subprocess.run([binary, "runner", "connect", "native-gate-lab"], env=controller, capture_output=True, text=True)
                (evidence / "connect-after-service-start.json").write_text(connection.stdout)
            if connection.returncode:
                raise RuntimeError("private native runner connection failed: " + connection.stdout + connection.stderr)
        connected = True
        (evidence / "connect.json").write_text(connection.stdout)
        controller.update(HOMEBOY_NATIVE_GATE_FIXTURE=str(root), HOMEBOY_NATIVE_GATE_RUST_SOURCE=rust_source,
                          HOMEBOY_NATIVE_GATE_BINARY=binary,
                          HOMEBOY_NATIVE_GATE_RECEIVER_BINARY=str(receiver_binary),
                          HOMEBOY_NATIVE_GATE_EVIDENCE=str(evidence), HOMEBOY_NATIVE_GATE_PROBE=str(probe_marker))
        completed = subprocess.run(["cargo", "test", "--quiet", "-p", "homeboy-lab-runner", "--lib",
            "gate_transport::native_tests::full_native_transport", "--", "--ignored", "--exact", "--test-threads=1", "--nocapture"],
            env=controller, capture_output=True, text=True)
        (evidence / "full-transport.stdout.txt").write_text(completed.stdout)
        (evidence / "full-transport.stderr.txt").write_text(completed.stderr)
        print(completed.stdout)
        if completed.returncode:
            print(completed.stderr, file=sys.stderr)
            raise RuntimeError("native full transport verification failed")
    finally:
        if Path(controller["HOMEBOY_ARTIFACT_ROOT"]).exists():
            shutil.copytree(controller["HOMEBOY_ARTIFACT_ROOT"], evidence / "custodied-artifacts", dirs_exist_ok=True)
        if connected:
            subprocess.run([binary, "runner", "disconnect", "native-gate-lab"], env=controller, capture_output=True)
        subprocess.run([binary, "daemon", "stop"], env=receiver, capture_output=True)
        # The native connector owns controller-scoped daemon state directories.
        # Stop only leases beneath this fixture, including failed boot attempts.
        for lease in (root / "receiver").rglob("state.json"):
            if "daemon-generations" not in lease.parts and lease.parent.name != "daemon":
                continue
            scoped = dict(receiver, HOMEBOY_DAEMON_STATE_DIR=str(lease.parent))
            subprocess.run([binary, "daemon", "stop"], env=scoped, capture_output=True)
        sshd.terminate()
        try:
            sshd.wait(timeout=10)
        except subprocess.TimeoutExpired:
            sshd.kill()
            sshd.wait()
        ssh_log.close()
