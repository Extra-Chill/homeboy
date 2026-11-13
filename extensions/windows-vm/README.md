# Windows VM

Standalone disposable `windows / x86_64 / vm` environment provider. No Studio-specific paths or core placement support required. The CLI emits one JSON result on stdout; failures have `ok:false`, a typed `error.type`, and nonzero status. `environment_providers` is provider metadata, not a promise that older core versions discover or place these environments automatically. `prepare` writes `runner.json` describing the SSH endpoint and declared environment; typed placement belongs to core (#17905).

## Host prerequisites

Linux x64 with working `/dev/kvm`, `/dev/net/tun`, Docker, Node (18+), OpenSSH ssh/scp, GNU cp and sha256sum. The Docker user must be authorized to run containers with KVM and NET_ADMIN. Internet access is needed for the Windows evaluation image, OpenSSH capability, Git and Node LTS. Use a dedicated trusted host: Docker access is privileged. Ports bind **only 127.0.0.1** (SSH 2222, RDP 3389, console 8006 by default). No restart policy is installed.

First install downloads the Windows ISO (about 25 minutes on the Lab) and then installs (about 45 minutes); the default SSH readiness timeout is 7200 seconds and covers both. The OEM bootstrap runs `-NonInteractive`, so a script error fails instead of waiting at a prompt. Cached snapshot boot should reach SSH within two minutes on the Lab; this is a hardware acceptance target, not an untested guarantee. CI does not start a VM.

## Configuration

Create a private JSON file and set `WINDOWS_VM_CONFIG` to its absolute path (or use `--config file` on each command):

```json
{
  "directory": "/srv/homeboy/windows-lease",
  "name": "homeboy-windows-vm",
  "adminUser": "vmadmin",
  "standardUser": "vmuser",
  "sshKey": "/secure/windows-vm-key",
  "adminPasswordEnv": "WINDOWS_VM_ADMIN_PASSWORD",
  "standardPasswordEnv": "WINDOWS_VM_STANDARD_PASSWORD",
  "image": "dockurr/windows:latest",
  "version": "11",
  "cores": 4,
  "ram": "8G",
  "disk": "64G",
  "port": 2222,
  "rdpPort": 3389,
  "consolePort": 8006,
  "sshTimeout": 7200,
  "artifactDirectory": "/srv/homeboy/windows-artifacts"
}
```

Supply passwords through those environment variables, not command arguments. Use a fresh dedicated key pair; public key defaults to `<sshKey>.pub` (override `publicKey`). Pin `image` to a Docker digest for reproducibility. Paths are host paths; choose a dedicated directory, never an existing data directory. The provider deletes its `storage` during reset/teardown. Protect the directory and artifact directory from other users. The private OEM JSON contains the standard password, and Docker metadata contains the initial administrator password; trusted host administrators can read them. Neither is printed in summaries or placed in runner.json. Snapshots and artifacts may contain secrets.

OEM bootstrap installs the SSH administrator key with SYSTEM/Administrators-only ACLs, PowerShell as default shell, a separate local non-admin user, Git and Node LTS. It does not install Homeboy: this implementation uses the SSH exec target. Installer/bootstrap failure prevents SSH readiness rather than silently declaring the image ready. Version/hash identity includes the Windows OS build, Docker image digest and SHA-256 of the stopped snapshot disk. Disk hashing adds time to snapshot creation, not cached prepare.

## Commands

Replace `windows-vm` below with `node /path/to/windows-vm/scripts/windows-vm.mjs`:

```sh
windows-vm prepare
windows-vm run --as standard --command 'whoami' --timeout 60
windows-vm run --as standard --script ./test.ps1 --artifact 'C:\results\report.json' --artifact 'C:\results\logs' --timeout 300 --reboot-cold
windows-vm run --as admin --command 'Get-Service sshd'
windows-vm reboot-cold
windows-vm snapshot
windows-vm reset
windows-vm teardown
```

- `prepare`: create/boot, or restore the base snapshot, wait for SSH, record the Windows build and lease identity (the lease expires 89 days after install) and write runner config. First successful prepare caches a stopped base then boots it. Existing snapshots are restored before every subsequent lease. Cached images are rebuilt conservatively after 89 days, before the 90-day evaluation limit; a guest-reported inactive license also triggers one clean rebuild. An inactive fresh install fails as `image_expired` rather than looping.
- `reboot-cold`: Docker restart and bounded SSH readiness wait. Also available before a run with `--reboot-cold`.
- `run`: requires explicit `--as standard|admin` and exactly one `--command` (PowerShell source) or local `--script`. Password task logon works without a desktop session. `--interactive` switches to an Interactive scheduled task for GUI workloads; **the selected user must already be logged into a desktop**, otherwise the run fails/times out. Explicit `exit N` in scripts determines status; scripts must propagate native command failures. For `--command`, the final native exit code is propagated automatically.
- `snapshot`: stop the VM and cache the entire storage directory (including `data.img` and firmware variables) using sparse copies. Leaves it stopped. Refreshes the disk hash but never extends the evaluation expiry.
- `reset`: stop and restore the base storage; leave stopped (call prepare to lease/boot). A file written after snapshot must be absent after reset + prepare.
- `teardown`: stop/remove the container and discard live storage, retaining the base image and evidence. Reports stopped/removal outcome; Docker failure is not a successful teardown. A second prepare restores cached storage and recreates the container as needed.

Lifecycle and run operations are serialized with an exclusive directory lock. After an unclean host termination, check that no operation/task is active before removing `operation.lock`. SSH keys are TOFU-pinned in a lease-private known_hosts file (removed only on image rebuild); do not reuse this endpoint for unrelated machines.

## Output freshness and cleanup

Every invocation generates an unpredictable run ID and independent nonce. The guest must create `C:\runs\<run-id>` exclusively and create its output with CreateNew. No previous output is removed or reused. Guest start/creation timestamps avoid host/guest clock skew; verification binds both to the host request start marker and nonce. Success requires fresh output, a final `EXIT:0 NONCE:<nonce>` trailer, matching result metadata, and every requested artifact copied locally. Missing output/trailer/result, nonce mismatch, stale timestamps, nonzero exit, missing artifact, task/transport failure and timeout are typed nonzero errors, never successful `NO OUTPUT` text.

The scheduled task runs with Limited rights for the standard account (administrators are rejected; the bootstrap grants only `SeBatchLogonRight`, which client Windows otherwise reserves for administrators), Highest for admin. Its worker enters a kernel job with KILL_ON_JOB_CLOSE before launching the payload, so child processes, including detached descendants, die on task exit or timeout. The task is stopped/unregistered in finally; on transport failure the host attempts a second cleanup SSH call and reports `cleanup_failed` if it cannot confirm cleanup. No networked provider can guarantee cleanup during host loss: use teardown/reset after a lost connection. Unique guest run directories remain for diagnosis until lease reset. Artifacts are staged while the task completes and copied to `<artifactDirectory>/<run-id>/artifacts/<index>`; the JSON summary maps each declared path to its local path and includes run/user, exit code, total duration and image identity. Transport/helper failures may leave partial local evidence but never produce a success summary.

## Validation

Fixture tests exercise valid output, stale output, nonce mismatch, missing/invalid trailers/results, nonzero exit and missing artifacts without a VM. On a KVM host, additionally validate prepare → SSH OSVersion, snapshot → write file → reset → prepare → file absent, teardown → no running container, and timed second prepare under two minutes. Real-VM behavior and GUI-session behavior require this host acceptance pass.
