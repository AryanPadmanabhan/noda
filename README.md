# noda

`noda` is a self-hosted OTA control plane and node agent for fleets that manage their own artifacts.

It is built around three ideas:

- the customer owns the build pipeline and artifact location
- the server owns intent, targeting, rollout state, and command issuance
- the node agent owns install, activation, validation, and rollback on the machine

`noda` is not a hosted artifact service. It does not require a vendor-managed build pipeline. It coordinates rollout of artifacts that already exist elsewhere.

## Current model

Today the project supports these executor families:

- `nix_generation`
- `grub_ab`
- `scripted`
- `noop`

The manifest schema is executor-specific. Nix releases only carry Nix fields. A/B releases only carry A/B fields. The old shared "artifact for everything" shape is gone for Nix flows.

## Architecture

There are two runtime components:

- `noda server`
  - stores releases, assets, deployments, commands, and command results in SQLite
  - exposes the HTTP API
  - decides which commands each asset should receive
- `noda agent`
  - checks in with the server
  - polls for commands
  - downloads or prepares artifacts as required by the executor
  - activates the new system
  - validates the post-activation state
  - performs rollback when policy requires it

At a high level:

1. A release is created.
2. A deployment targets a set of assets.
3. Assets poll and receive install commands.
4. The agent executes the executor-specific workflow.
5. The agent reports success or failure.
6. The server updates deployment and asset state.

## What is production-useful now

- self-hosted control plane
- label- and target-type-based rollout selection
- mission-state gating
- canary and max-parallel rollout controls
- artifact-driven Nix deployments using `nix copy --from`
- build-on-target Nix deployments for bootstrap flows
- post-boot validation
- automatic rollback for `nix_generation` after validation failure

## What is still intentionally incomplete

- authentication and authorization
- TLS / reverse-proxy packaging
- signed manifest and artifact trust model
- real bootloader-native `grub_ab` activation and rollback
- metrics / tracing export
- formal server install packages outside NixOS

This repo is now much closer to a product core than a demo, but those items still matter before a broad production rollout.

## Build

```bash
cargo build
```

## Run the server

```bash
cargo run -- server --bind 127.0.0.1:8080 --db noda.db
```

The only accepted panic-at-startup boundary is process startup. Runtime request handling and DB access are expected to return errors, not panic.

## Run an agent

```bash
cargo run -- agent \
  --server http://127.0.0.1:8080 \
  --asset-id node-1 \
  --asset-type edge-linux-aarch64 \
  --mission-state idle \
  --state-dir ./agent-state \
  --labels region=lab
```

## Nix-native onboarding

For NixOS-managed systems, the intended onboarding path is declarative.

### Node onboarding

1. Add this repo as a flake input in the node's own flake.
2. Import `noda.nixosModules.noda`.
3. Enable `services.noda`.
4. Rebuild once.

Example:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    noda.url = "github:AryanPadmanabhan/noda";
  };

  outputs = { nixpkgs, noda, ... }: {
    nixosConfigurations.node-1 = nixpkgs.lib.nixosSystem {
      system = "aarch64-linux";
      specialArgs = { inherit noda; };
      modules = [
        ./configuration.nix
        ./node-1.nix
        noda.nixosModules.noda
      ];
    };
  };
}
```

`node-1.nix`:

```nix
{ noda, pkgs, ... }:
{
  services.noda = {
    enable = true;
    package = noda.packages.${pkgs.system}.noda;
    serverUrl = "http://10.2.24.81:8080";
    assetId = "node-1";
    assetType = "edge-linux-aarch64";
    missionState = "idle";
    labels = [ "region=lab" ];
  };
}
```

Apply it:

```bash
cd /etc/nixos
sudo nixos-rebuild switch --flake .#node-1
systemctl status noda
```

### Control-plane onboarding on NixOS

1. Import `noda.nixosModules.noda-server`.
2. Enable `services.noda-server`.
3. Rebuild once.

Example:

```nix
{
  inputs.noda.url = "github:AryanPadmanabhan/noda";

  outputs = { nixpkgs, noda, ... }: {
    nixosConfigurations.control-plane = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        noda.nixosModules.noda-server
        ({ pkgs, ... }: {
          services.noda-server = {
            enable = true;
            package = noda.packages.${pkgs.system}.noda;
            bind = "0.0.0.0:8080";
          };
        })
      ];
    };
  };
}
```

Apply it:

```bash
sudo nixos-rebuild switch --flake .#control-plane
systemctl status noda-server
```

## API surface

### Server health

- `GET /healthz`

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

### Agent workflow

- `POST /v1/agent/poll`
- `POST /v1/agent/result`

Error responses are JSON objects of the form:

```json
{
  "code": "invalid_request",
  "message": "release target_type and selector target_type mismatch"
}
```

## Manifest model

Each release has:

- `target_type`
- `executor`
- `validation`
- `rollback`
- optional release metadata labels

The executor shape is tagged and typed.

### `nix_generation`

Supported sources:

- `build_flake`
- `copy_from_store`

Example: build on target

```json
{
  "version": "bootstrap-agent-1",
  "manifest": {
    "target_type": "edge-linux-aarch64",
    "executor": {
      "kind": "nix_generation",
      "source": {
        "kind": "build_flake",
        "flake": "/home/aryanp/noda",
        "flake_attr": "nixosConfigurations.ota-vm.config.system.build.toplevel"
      }
    },
    "validation": {
      "expected_hostname": "ota-vm",
      "timeout_seconds": 900,
      "health_checks": [
        {
          "name": "noda-active",
          "kind": "command_exit_zero",
          "command": "systemctl is-active noda"
        }
      ]
    },
    "rollback": {
      "automatic": true,
      "on_boot_failure": true,
      "on_validation_failure": true,
      "candidate_timeout_seconds": 900
    }
  }
}
```

Example: copy prebuilt system from a store source

```json
{
  "version": "ota-vm-noda-3",
  "manifest": {
    "target_type": "edge-linux-aarch64",
    "executor": {
      "kind": "nix_generation",
      "source": {
        "kind": "copy_from_store",
        "copy_from": "ssh://aryanpaddy@10.2.24.81",
        "store_path": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nixos-system-ota-vm-..."
      }
    },
    "validation": {
      "expected_hostname": "ota-vm",
      "timeout_seconds": 900,
      "health_checks": [
        {
          "name": "noda-active",
          "kind": "command_exit_zero",
          "command": "systemctl is-active noda"
        }
      ]
    }
  }
}
```

### `grub_ab`

`grub_ab` currently stages the artifact into the inactive slot area and can optionally run a user-supplied activation command. It is a scaffolding executor, not a full bootloader-integrated A/B implementation yet.

Example:

```json
{
  "version": "2.4.1",
  "manifest": {
    "target_type": "edge-linux-x86",
    "executor": {
      "kind": "grub_ab",
      "artifact": {
        "url": "https://customer-store.example.com/releases/edge-2.4.1.img.zst",
        "sha256": "deadbeef...",
        "headers": {}
      },
      "slot_pair": ["A", "B"]
    },
    "validation": {
      "health_checks": [
        {
          "name": "service-ready",
          "kind": "command_exit_zero",
          "command": "systemctl is-active my-service"
        }
      ]
    }
  }
}
```

### `scripted`

`scripted` is the escape hatch for integrating with an existing updater. It still accepts shell commands, but it is explicitly isolated to this executor instead of leaking shell fields into every release type.

Example:

```json
{
  "version": "scripted-1",
  "manifest": {
    "target_type": "edge-linux-x86",
    "executor": {
      "kind": "scripted",
      "artifact": {
        "url": "https://example.com/update.tar.gz",
        "sha256": "deadbeef...",
        "headers": {}
      },
      "install_command": "/usr/local/bin/install-update \"$ARTIFACT_PATH\"",
      "activate_command": "systemctl restart my-service"
    }
  }
}
```

## Validation

Validation is shared across executors.

Available checks:

- `expected_hostname`
- `expected_system_path`
- `health_checks`
- `timeout_seconds`

Health check kinds:

- `always_pass`
- `command_exit_zero`
- `http_get`

Validation runs after install for non-reboot flows and after reboot for `nix_generation`.

## Rollback

Rollback is policy-driven.

Current implemented path:

- `nix_generation` can roll back to the previous known-good system after post-boot validation failure or timeout

Current non-goals:

- full bootloader-native rollback for `grub_ab`
- indefinite retry loops

For Nix rollbacks, the agent persists the previous system path before activation, attempts the forward boot, validates it, and if validation fails it stages the previous generation and reboots again.

## Deployment model

A deployment contains:

- `release_id`
- `rollout_name`
- `selector`
- `strategy`

Example:

```json
{
  "release_id": "REPLACE_WITH_RELEASE_ID",
  "rollout_name": "lab-rollout",
  "selector": {
    "target_type": "edge-linux-aarch64",
    "labels": {
      "region": "lab"
    },
    "mission_states": ["idle"]
  },
  "strategy": {
    "canary": 1,
    "batch_size": 10,
    "max_parallel": 5,
    "max_failure_rate": 0.1,
    "require_idle": true
  }
}
```

## Example workflows

### Bootstrap a NixOS node by building on the node

Use:

- `examples/nix-build-on-target-release.json`
- `examples/nix-build-on-target-deployment.json`

This is useful when the node does not yet have the newer `noda` agent or when you want the machine to build from a local flake checkout.

### Deploy a prebuilt Nix system by copying from a store source

Use:

- `examples/nix-copy-release.json`
- `examples/nix-copy-deployment.json`

This is the artifact-driven Nix path. The node copies a prebuilt `/nix/store/...` system from a reachable store source such as another machine over SSH.

### Generic A/B-style artifact rollout

Use:

- `examples/basic-release.json`
- `examples/basic-deployment.json`

This is the minimal non-Nix example in the repo today.

## VM workflow that has been exercised

The repo has already been used in a practical Nix VM flow:

1. bootstrap the VM onto a new `noda` agent
2. build a new NixOS toplevel
3. copy the toplevel into a temporary store source
4. submit a `nix_generation` release with `copy_from_store`
5. let the node `nix copy --from ...`
6. reboot into the new system
7. validate the booted system
8. trigger rollback on a bad validation configuration

That path is the current strongest demonstrated use case.

## Repository layout

- `src/main.rs`
  - CLI entrypoint
- `src/server/mod.rs`
  - server bootstrap and shared app state
- `src/api/`
  - HTTP routes and API error handling
- `src/db/`
  - SQLite connection, migrations, and repositories
- `src/executors/`
  - executor implementations and dispatch
- `src/agent/mod.rs`
  - node polling loop, local state, validation, and rollback workflow
- `src/types.rs`
  - wire types and manifest schema
- `examples/`
  - release/deployment examples and Nix enrollment example

## Development notes

- runtime code in `src/` should not use `unwrap()` / `expect()` / `panic!()` for normal control flow
- shell-based behavior is confined to the executors that actually require it
- API handlers return structured JSON errors instead of panicking on poisoned locks
- the DB layer and executor layer are split into smaller modules to keep growth manageable

## Verification

Useful local checks:

```bash
cargo check
cargo test --no-run
```

Full `cargo test` starts local test servers and agents. In restricted sandboxes that can fail due local bind permissions even when the code is correct.
