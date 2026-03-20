use super::{
    artifact_path, run_shell, shell_env, ActivationOutcome, ExecutionContext, Executor,
    PendingReboot, RollbackAction,
};
use crate::types::{GrubAbCompression, GrubAbExecutorSpec, GrubAbSlot};
use anyhow::{anyhow, bail, Context, Result};
use std::{
    env, fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
};

pub(super) struct GrubAbExecutor;

impl Executor for GrubAbExecutor {
    fn install<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::grub_ab_spec(ctx)?;
            if let Some(slots) = &spec.slots {
                let (_, next_slot) = ordered_slots(slots, &ctx.current_slot, &ctx.next_slot)?;
                let source_artifact =
                    artifact_path(ctx).context("grub-ab requires a downloaded artifact path")?;
                let image_path = prepare_image(spec, source_artifact, &ctx.state_dir)?;
                write_image_to_device(&image_path, &next_slot.device)?;
            } else {
                let slots_dir = ctx.state_dir.join("slots");
                fs::create_dir_all(&slots_dir)?;
                let source_artifact =
                    artifact_path(ctx).context("grub-ab requires a downloaded artifact path")?;
                let dest = slots_dir.join(format!("slot-{}-{}", ctx.next_slot, ctx.release_version));
                fs::copy(source_artifact, &dest)
                    .with_context(|| format!("copying artifact into inactive slot {:?}", dest))?;
            }
            Ok(())
        })
    }

    fn activate<'a>(
        &'a self,
        ctx: &'a ExecutionContext,
    ) -> Pin<Box<dyn Future<Output = Result<ActivationOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = super::grub_ab_spec(ctx)?;
            if let Some(slots) = &spec.slots {
                let (_, next_slot) = ordered_slots(slots, &ctx.current_slot, &ctx.next_slot)?;
                set_grub_saved_entry(&spec.grubenv_path, &next_slot.grub_menu_entry)?;
                if let Some(command) = &spec.activate_command {
                    run_shell(command, &shell_env(ctx, &[]))?;
                }
                request_reboot()?;
                Ok(ActivationOutcome::AwaitReboot(PendingReboot {
                    expected_system_path: None,
                    expected_active_slot: Some(next_slot.name.clone()),
                    expected_root_device: Some(next_slot.device.clone()),
                }))
            } else {
                if let Some(command) = &spec.activate_command {
                    run_shell(command, &shell_env(ctx, &[]))?;
                } else {
                    fs::write(ctx.state_dir.join("next-boot-slot"), &ctx.next_slot)?;
                }
                Ok(ActivationOutcome::Complete)
            }
        })
    }
}

pub fn detect_active_slot(spec: &GrubAbExecutorSpec) -> Result<String> {
    let slots = spec
        .slots
        .as_ref()
        .context("grub_ab.slots is required for active slot detection")?;
    let active_device = current_root_device()?;
    for slot in slots {
        if same_device(&active_device, &slot.device)? {
            return Ok(slot.name.clone());
        }
    }
    Err(anyhow!(
        "active root device {} does not match any grub_ab slot device",
        active_device
    ))
}

pub fn rollback_action(spec: &GrubAbExecutorSpec, current_slot: &str) -> Result<RollbackAction> {
    let slots = spec
        .slots
        .as_ref()
        .context("grub_ab.slots is required for rollback")?;
    let current = slots
        .iter()
        .find(|slot| slot.name == current_slot)
        .with_context(|| format!("unknown current grub_ab slot: {current_slot}"))?;
    Ok(RollbackAction::GrubAb {
        grubenv_path: spec.grubenv_path.clone(),
        previous_grub_menu_entry: current.grub_menu_entry.clone(),
    })
}

pub fn rollback_grub_ab(grubenv_path: &str, previous_grub_menu_entry: &str) -> Result<()> {
    set_grub_saved_entry(grubenv_path, previous_grub_menu_entry)?;
    request_reboot()
}

fn ordered_slots<'a>(
    slots: &'a [GrubAbSlot; 2],
    current_slot: &str,
    next_slot: &str,
) -> Result<(&'a GrubAbSlot, &'a GrubAbSlot)> {
    let current = slots
        .iter()
        .find(|slot| slot.name == current_slot)
        .with_context(|| format!("unknown current grub_ab slot: {current_slot}"))?;
    let next = slots
        .iter()
        .find(|slot| slot.name == next_slot)
        .with_context(|| format!("unknown next grub_ab slot: {next_slot}"))?;
    Ok((current, next))
}

fn prepare_image(spec: &GrubAbExecutorSpec, artifact_path: &Path, state_dir: &Path) -> Result<PathBuf> {
    match resolve_compression(spec, artifact_path) {
        GrubAbCompression::None => Ok(artifact_path.to_path_buf()),
        GrubAbCompression::Zstd => decompress_zstd(artifact_path, state_dir),
        GrubAbCompression::Auto => unreachable!("auto is resolved before use"),
    }
}

fn resolve_compression(spec: &GrubAbExecutorSpec, artifact_path: &Path) -> GrubAbCompression {
    match spec.compression {
        GrubAbCompression::Auto => artifact_path
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| ext.eq_ignore_ascii_case("zst"))
            .map(|_| GrubAbCompression::Zstd)
            .unwrap_or(GrubAbCompression::None),
        compression => compression,
    }
}

fn decompress_zstd(artifact_path: &Path, state_dir: &Path) -> Result<PathBuf> {
    let dest = state_dir.join("artifacts").join("grub-ab-image.ext4");
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let status = Command::new(zstd_command())
        .args(["-d", "-f"])
        .arg(artifact_path)
        .args(["-o"])
        .arg(&dest)
        .status()
        .with_context(|| format!("running zstd to decompress {}", artifact_path.display()))?;
    if !status.success() {
        bail!("zstd decompression failed for {}", artifact_path.display());
    }
    Ok(dest)
}

fn write_image_to_device(image_path: &Path, device: &str) -> Result<()> {
    let status = Command::new(dd_command())
        .arg(format!("if={}", image_path.display()))
        .arg(format!("of={device}"))
        .arg("bs=16M")
        .arg("conv=fsync")
        .arg("status=none")
        .status()
        .with_context(|| format!("writing {} to {}", image_path.display(), device))?;
    if !status.success() {
        bail!("dd failed while writing {} to {}", image_path.display(), device);
    }
    Ok(())
}

fn set_grub_saved_entry(grubenv_path: &str, entry: &str) -> Result<()> {
    let status = Command::new(grub_editenv_command())
        .arg(grubenv_path)
        .arg("set")
        .arg(format!("saved_entry={entry}"))
        .status()
        .with_context(|| format!("running grub-editenv for {}", grubenv_path))?;
    if !status.success() {
        bail!("grub-editenv failed while setting saved_entry={entry}");
    }
    Ok(())
}

fn request_reboot() -> Result<()> {
    let status = Command::new(reboot_command())
        .arg("reboot")
        .status()
        .context("requesting reboot")?;
    if !status.success() {
        bail!("reboot command failed");
    }
    Ok(())
}

fn current_root_device() -> Result<String> {
    if let Ok(path) = env::var("NODA_GRUB_AB_ACTIVE_DEVICE_FILE") {
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }
    let output = Command::new("findmnt")
        .args(["-n", "-o", "SOURCE", "/"])
        .output()
        .context("running findmnt to detect current root device")?;
    if !output.status.success() {
        bail!("findmnt failed while detecting current root device");
    }
    let source = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if source.is_empty() {
        bail!("findmnt did not return a current root device");
    }
    Ok(source)
}

fn same_device(left: &str, right: &str) -> Result<bool> {
    let left = canonicalize_device(left)?;
    let right = canonicalize_device(right)?;
    Ok(left == right)
}

fn canonicalize_device(path: &str) -> Result<String> {
    let device_path = Path::new(path);
    if device_path.exists() {
        return Ok(fs::canonicalize(device_path)?.display().to_string());
    }
    Ok(path.to_string())
}

fn grub_editenv_command() -> String {
    env::var("NODA_GRUB_EDITENV").unwrap_or_else(|_| "grub-editenv".into())
}

fn reboot_command() -> String {
    env::var("NODA_REBOOT_COMMAND").unwrap_or_else(|_| "systemctl".into())
}

fn dd_command() -> String {
    env::var("NODA_DD_COMMAND").unwrap_or_else(|_| "dd".into())
}

fn zstd_command() -> String {
    env::var("NODA_ZSTD_COMMAND").unwrap_or_else(|_| "zstd".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ArtifactSource, GrubAbCompression};
    use std::collections::BTreeMap;

    fn spec() -> GrubAbExecutorSpec {
        GrubAbExecutorSpec {
            artifact: ArtifactSource {
                url: "file:///tmp/image.ext4.zst".into(),
                sha256: None,
                headers: BTreeMap::new(),
            },
            slot_pair: Some(["A".into(), "B".into()]),
            slots: Some([
                GrubAbSlot {
                    name: "A".into(),
                    device: "/dev/disk/by-partlabel/root-a".into(),
                    grub_menu_entry: "noda-slot-a".into(),
                },
                GrubAbSlot {
                    name: "B".into(),
                    device: "/dev/disk/by-partlabel/root-b".into(),
                    grub_menu_entry: "noda-slot-b".into(),
                },
            ]),
            grubenv_path: "/boot/grub/grubenv".into(),
            compression: GrubAbCompression::Auto,
            activate_command: None,
        }
    }

    #[test]
    fn rollback_action_uses_current_slot_entry() {
        let action = rollback_action(&spec(), "A").expect("rollback action");
        let RollbackAction::GrubAb {
            grubenv_path,
            previous_grub_menu_entry,
        } = action
        else {
            panic!("expected grub rollback action");
        };
        assert_eq!(grubenv_path, "/boot/grub/grubenv");
        assert_eq!(previous_grub_menu_entry, "noda-slot-a");
    }

    #[test]
    fn auto_compression_detects_zstd() {
        let resolved = resolve_compression(&spec(), Path::new("/tmp/rootfs.ext4.zst"));
        assert!(matches!(resolved, GrubAbCompression::Zstd));
    }
}
