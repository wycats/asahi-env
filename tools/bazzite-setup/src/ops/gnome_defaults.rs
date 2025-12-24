use crate::ops::util;
use anyhow::{Context, Result};

pub fn check(_allow_sudo: bool) -> Result<()> {
    println!("== GNOME defaults ==");

    // If gsettings isn't available, keep this non-fatal.
    if util::gsettings_try_get("org.gnome.desktop.interface", "show-battery-percentage")?.is_none()
    {
        println!("GNOME gsettings not available (skipping)");
        return Ok(());
    }

    check_key(
        "org.gnome.desktop.peripherals.touchpad",
        "tap-and-drag",
        "true",
    )?;
    check_key(
        "org.gnome.desktop.peripherals.touchpad",
        "tap-and-drag-lock",
        "true",
    )?;
    check_key(
        "org.gnome.desktop.peripherals.touchpad",
        "accel-profile",
        "'flat'",
    )?;
    check_key(
        "org.gnome.desktop.peripherals.touchpad",
        "disable-while-typing",
        "true",
    )?;

    check_key("org.gnome.mutter", "edge-tiling", "false")?;

    check_key(
        "org.gnome.desktop.interface",
        "show-battery-percentage",
        "true",
    )?;

    check_key("org.gnome.software", "download-updates-on-metered", "true")?;

    Ok(())
}

pub fn apply(_allow_sudo: bool, dry_run: bool) -> Result<()> {
    println!("== Apply GNOME defaults ==");

    if util::gsettings_try_get("org.gnome.desktop.interface", "show-battery-percentage")?.is_none()
    {
        println!("GNOME gsettings not available (skipping)");
        return Ok(());
    }

    set_key(
        "org.gnome.desktop.peripherals.touchpad",
        "tap-and-drag",
        "true",
        dry_run,
    )?;
    set_key(
        "org.gnome.desktop.peripherals.touchpad",
        "tap-and-drag-lock",
        "true",
        dry_run,
    )?;
    set_key(
        "org.gnome.desktop.peripherals.touchpad",
        "accel-profile",
        "'flat'",
        dry_run,
    )?;
    set_key(
        "org.gnome.desktop.peripherals.touchpad",
        "disable-while-typing",
        "true",
        dry_run,
    )?;

    set_key("org.gnome.mutter", "edge-tiling", "false", dry_run)?;

    set_key(
        "org.gnome.desktop.interface",
        "show-battery-percentage",
        "true",
        dry_run,
    )?;

    set_key(
        "org.gnome.software",
        "download-updates-on-metered",
        "true",
        dry_run,
    )?;

    Ok(())
}

fn check_key(schema: &str, key: &str, desired: &str) -> Result<()> {
    let Some(current) = util::gsettings_try_get(schema, key)? else {
        println!("gsettings: {schema} {key}: <unavailable> (skipping)");
        return Ok(());
    };
    if current == desired {
        println!("gsettings: {schema} {key} already {desired}");
    } else {
        println!("gsettings: {schema} {key}: {current} -> {desired}");
    }
    Ok(())
}

fn set_key(schema: &str, key: &str, desired: &str, dry_run: bool) -> Result<()> {
    let Some(current) = util::gsettings_try_get(schema, key)? else {
        println!("gsettings: {schema} {key}: <unavailable> (skipping)");
        return Ok(());
    };
    if current == desired {
        println!("gsettings: {schema} {key} already {desired}");
        return Ok(());
    }

    println!("gsettings: {schema} {key}: {current} -> {desired}");
    util::gsettings_set(schema, key, desired, dry_run)
        .with_context(|| format!("gsettings set {schema} {key}"))?;
    Ok(())
}
