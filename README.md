# noda

A self-hosted **OTA orchestrator** and **target-side agent** for customer-owned artifacts.

This project does **not** build artifacts and does **not** require vendor-hosted storage.
Instead, it handles:

- release registration from external artifact URLs
- fleet inventory
- deployment targeting by asset type and labels
- mission-aware gating (`idle` vs non-idle in v1)
- agent polling and command issuance
- install + activation through pluggable executors
- post-activation health checks
- deployment success / failure tracking
- automatic deployment-wide halt when failure rate exceeds policy

## Current scope

This is intentionally focused on the strongest MVP boundary:

- **customer owns** build pipeline, artifact store, and release contents
- **this project owns** OTA intent, activation flow, validation, and rollback signaling

## Included executors

- `noop` — no-op executor for local testing
- `scripted` — shell-command-based executor for integration with an existing updater
- `grub-ab` — pragmatic A/B-style demo executor that stages artifacts into an inactive slot directory and records the next boot target; this can be swapped for real `grub-editenv` integration

## What it is ready for

- self-hosted lab deployments
- QEMU demos
- proving the control-plane / agent / manifest model
- wiring into a real A/B installer with minimal code changes

## What is not yet fully production-complete

This repo is designed to be a solid starting point, but I have to be honest about what is still left for a hard-production rollout:

- real `grub-editenv` or bootloader-native rollback confirmation
- durable authn/authz and multi-user auth
- TLS termination and API auth tokens
- signed manifests beyond checksum verification
- richer mission policy, soak windows, and approvals
- first-class metrics / tracing export

Those are clean next steps rather than architectural rewrites.

## Build

```bash
cargo build
```

## Run the server

```bash
cargo run -- server --bind 127.0.0.1:8080 --db noda.db
```

## Run an agent

```bash
cargo run -- agent \
  --server http://127.0.0.1:8080 \
  --asset-id node-1 \
  --asset-type edge-linux-x86 \
  --mission-state idle \
  --labels region=lab
```

## Nix-native enrollment

For NixOS-managed nodes, the intended onboarding path is:

1. Add this repo as a flake input in the user's own flake.
2. Import `noda.nixosModules.noda`.
3. Enable `services.noda`.
4. Rebuild the host once.

Example host snippet:

```nix
{
  inputs.noda.url = "github:YOUR_ORG/noda";

  outputs = { self, nixpkgs, noda, ... }: {
    nixosConfigurations.node-1 = nixpkgs.lib.nixosSystem {
      system = "aarch64-linux";
      modules = [
        ./hardware-configuration.nix
        ./node-1.nix
        noda.nixosModules.noda
      ];
    };
  };
}
```

```nix
{ noda, pkgs, ... }:
{
  services.noda = {
    enable = true;
    package = noda.packages.${pkgs.system}.noda;
    serverUrl = "http://127.0.0.1:8080";
    assetId = "node-1";
    assetType = "edge-linux-aarch64";
    labels = [ "region=lab" ];
  };
}
```

After rebuild:

```bash
sudo nixos-rebuild switch --flake .#node-1
systemctl status noda
```

See the `examples/nix-native-enrollment` example for a minimal local setup.

## API surface

### Releases

- `POST /v1/releases`
- `GET /v1/releases`
- `GET /v1/releases/:id`

### Assets

- `GET /v1/assets`
- `GET /v1/assets/:id`
- `POST /v1/agent/checkin`

### Deployments

- `POST /v1/deployments`
- `GET /v1/deployments`
- `GET /v1/deployments/:id`
- `GET /v1/deployments/:id/targets`
- `POST /v1/deployments/:id/pause`
- `POST /v1/deployments/:id/abort`

### Agent flow

- `POST /v1/agent/poll`
- `POST /v1/agent/result`

## Manifest example

```json
{
  "version": "2.4.1",
  "manifest": {
    "target_type": "edge-linux-x86",
    "artifact": {
      "url": "https://customer-store.example.com/releases/edge-2.4.1.img.zst",
      "sha256": "..."
    },
    "install": {
      "install_type": "ab-image",
      "executor": "grub-ab",
      "slot_pair": ["A", "B"]
    },
    "activation": {
      "activation_type": "bootloader-switch",
      "bootloader": "grub"
    },
    "rollback": {
      "automatic": true,
      "on_boot_failure": true,
      "on_health_failure": true,
      "candidate_timeout_seconds": 900
    },
    "health_checks": [
      {
        "name": "service-ready",
        "kind": "command_exit_zero",
        "command": "systemctl is-active my-service"
      }
    ]
  }
}
```

## Deployment example

```json
{
  "release_id": "<release-id>",
  "rollout_name": "lab-rollout",
  "selector": {
    "target_type": "edge-linux-x86",
    "labels": {
      "region": "lab"
    },
    "mission_states": ["idle"]
  },
  "strategy": {
    "canary": 1,
    "batch_size": 10,
    "max_parallel": 2,
    "max_failure_rate": 0.2,
    "require_idle": true
  }
}
```

## Design choices

### Why SQLite?

For a self-hosted OSS MVP, SQLite keeps setup friction low and avoids unnecessary ops burden. The code is structured so you can replace the DB layer later without rewriting the rest of the control flow.

### Why pull-based agents?

Pull is simpler and safer for constrained or NATed environments and keeps the control plane stateless at the edge.

### Why executors?

The control plane should stay generic. The executor owns the install/activate details, so you can support:

- raw A/B images
- RAUC
- Mender
- `grub-editenv`
- custom shell-based install flows

without changing deployment orchestration.

## Suggested next steps

1. replace `grub-ab` file-based activation with real `grub-editenv` integration
2. add signed API tokens for agents
3. add deployment soak periods and validation windows
4. add webhook / Prometheus integration
5. add a small web UI
6. add a QEMU-based end-to-end test harness

## Repo layout

```text
src/
  api/
  agent/
  db/
  executors/
  server/
  types.rs
examples/
  create_release.json
  create_deployment.json
scripts/
  demo-health.sh
```
# noda
