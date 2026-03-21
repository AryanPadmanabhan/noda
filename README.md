# noda

`noda` is a self-hosted OS OTA control plane and device agent for self-managed artifacts.

It is built around three ideas:

- BYO artifacts
- controller owns releases and deployments
- the device agent owns install, activation, validation, and rollback

## Current model

Today the project supports these executor families:

- `nix_generation`
- `grub_ab`
- `mock`

The manifest schema is executor-specific. Nix releases only carry Nix fields. A/B releases only carry A/B fields.

## Architecture

There are two runtime components:

- `noda server`
  - stores releases, assets, deployments, and deployment results
  - decides which deployments each asset should receive
- `noda agent`
  - checks in with the server
  - polls for deployments
  - downloads or prepares artifacts as required by the executor
  - activates the new system
  - validates the post-activation state
  - performs rollback when policy requires it

Flow:

1. A release is created.
2. A deployment targets a set of assets.
3. Assets poll and receive install commands.
4. The agent executes the executor-specific workflow.
5. The agent reports success or failure.
6. The server updates deployment and asset state.

## Current state 

- self-hosted controller
- targeted rollouts
- canary and max-parallel rollout controls
- artifact-driven Nix deployments using `nix copy --from`
- build-on-target Nix deployments for bootstraping
- GRUB-based A/B rootfs deployments using raw `ext4` or `ext4.zst` artifacts
- post-boot validation
- automatic rollback after validation failure
- automatic healthchecks 

## TODOs

- authentication and authorization
- streaming SHA256 verification for large rootfs images
- probe / preflight tooling for `grub_ab` machine setup
- metrics / tracing export
- formal server install packages outside NixOS

## Run the server

```bash
cargo run -- server --bind 127.0.0.1:8080 --db noda.db
```

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

### Device onboarding

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
    serverUrl = "http://{SERVER_NODE_IP}:{SERVER_PORT}";
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
sudo nixos-rebuild boot --flake .#node-1
systemctl reboot
systemctl status noda
```

### Server onboarding on NixOS

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
sudo nixos-rebuild boot --flake .#control-plane
systemctl reboot
systemctl status noda-server
```

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

`nix_generation` now supports one source:

- `copy_from_store`

Example: copy a prebuilt system from a store source

```json
{
  "version": "ota-vm-noda-3",
  "manifest": {
    "target_type": "edge-linux-aarch64",
    "executor": {
      "kind": "nix_generation",
      "source": {
        "kind": "copy_from_store",
        "copy_from": "ssh://{USERNAME}@{HOST}",
        "store_path": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-nixos-system-node-1-..."
      }
    },
    "validation": {
      "expected_hostname": "node-1",
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

`grub_ab` is the non-Nix A/B executor. The contract is narrow:

- the user provides a bootable rootfs image
- supported artifact formats are raw `ext4` and `ext4.zst`
- the machine already has two root partitions and a working GRUB config
- the release declares both slot devices and the GRUB menu entry for each slot
- the release declares the target filesystem label for each slot
- the agent writes the image into the inactive root partition
- the agent runs `e2fsck`, restores the expected filesystem label, and randomizes the inactive-slot UUID
- the agent sets `saved_entry` in `grubenv`
- the machine reboots
- post-boot validation runs
- rollback sets `saved_entry` back to the previous slot and reboots again

The executor detects the currently booted slot by comparing the active root device from `findmnt -n -o SOURCE /` against the configured slot devices.

#### Operating System requirements

`grub_ab` is currently intended for Linux systems that already match a prepared A/B boot layout. The supported shape today is:

- Linux with GRUB as the bootloader
- an EFI system partition
- two rootfs partitions, one for slot A and one for slot B
- one persistent data partition shared by both slots
- label-based boot entries such as `rootfs-a` and `rootfs-b`
- a known authoritative `grubenv` location, typically on the slot A filesystem
- `GRUB_DEFAULT=saved` and `GRUB_SAVEDEFAULT=true`
- `grub-editenv`, `e2fsck`, `e2label`, and `tune2fs` available on the device
- a bootable rootfs artifact that already contains the intended OS and payload

Example:

```json
{
  "version": "2.4.1",
  "manifest": {
    "target_type": "edge-linux-x86",
    "executor": {
      "kind": "grub_ab",
      "artifact": {
        "url": "https://customer-store.example.com/releases/edge-2.4.1.ext4.zst",
        "sha256": "deadbeef...",
        "headers": {}
      },
      "slot_pair": ["A", "B"],
      "slots": [
        {
          "name": "A",
          "device": "/dev/disk/by-partlabel/rootfs-a",
          "grub_menu_entry": "noda-slot-a",
          "filesystem_label": "rootfs-a"
        },
        {
          "name": "B",
          "device": "/dev/disk/by-partlabel/rootfs-b",
          "grub_menu_entry": "noda-slot-b",
          "filesystem_label": "rootfs-b"
        }
      ],
      "boot_control": {
        "authority_device": "/dev/disk/by-partlabel/rootfs-a",
        "mountpoint": "/mnt/noda-bootctl",
        "grubenv_relpath": "/boot/grub/grubenv"
      },
      "compression": "auto"
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

### Deploy a prebuilt Nix system by copying from a store source

Use:

- `examples/nix-copy-release.json`
- `examples/nix-copy-deployment.json`


### Generic A/B-style artifact rollout

Use:

- `examples/basic-release.json`
- `examples/basic-deployment.json`