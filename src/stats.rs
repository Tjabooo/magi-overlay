//! Live system stats (Linux sysfs/procfs): CPU, RAM, AMD GPU. 1 Hz, no subprocesses.

use crate::hub::Hub;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn read_u64(p: &Path) -> Option<u64> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn hwmon_named(names: &[&str]) -> Option<PathBuf> {
    fs::read_dir("/sys/class/hwmon")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            fs::read_to_string(p.join("name"))
                .map(|n| names.contains(&n.trim()))
                .unwrap_or(false)
        })
}

/// The amdgpu device with the most VRAM (skips the iGPU).
fn gpu_dir() -> Option<PathBuf> {
    fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.path().join("device"))
        .filter(|d| d.join("gpu_busy_percent").exists())
        .max_by_key(|d| read_u64(&d.join("mem_info_vram_total")).unwrap_or(0))
}

fn cpu_times() -> Option<(u64, u64)> {
    let s = fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().next()?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    let idle = v.get(3)? + v.get(4).unwrap_or(&0);
    Some((v.iter().sum(), idle))
}

fn meminfo() -> Option<(u64, u64)> {
    let s = fs::read_to_string("/proc/meminfo").ok()?;
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let total = get("MemTotal:")?;
    Some((total - get("MemAvailable:")?, total))
}

pub async fn run(hub: Arc<Hub>) {
    let gpu = gpu_dir();
    let gpu_hwmon = gpu.as_ref().and_then(|g| {
        fs::read_dir(g.join("hwmon"))
            .ok()?
            .flatten()
            .next()
            .map(|e| e.path())
    });
    let cpu_hwmon = hwmon_named(&["k10temp", "coretemp", "zenpower"]);
    let mut prev = cpu_times();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let mut v: Value = json!({});
        let now = cpu_times();
        if let (Some((t0, i0)), Some((t1, i1))) = (prev, now) {
            let dt = t1.saturating_sub(t0).max(1);
            v["cpu"] = (100 - (i1.saturating_sub(i0) * 100 / dt)).into();
        }
        prev = now;
        if let Some(h) = &cpu_hwmon {
            v["cpu_temp"] = read_u64(&h.join("temp1_input")).map(|t| t / 1000).into();
        }
        if let Some((used, total)) = meminfo() {
            v["ram_used"] = ((used as f64 / 1048576.0 * 10.0).round() / 10.0).into();
            v["ram_total"] = ((total as f64 / 1048576.0).round()).into();
        }
        if let Some(g) = &gpu {
            v["gpu"] = read_u64(&g.join("gpu_busy_percent")).into();
            v["vram_used"] = read_u64(&g.join("mem_info_vram_used"))
                .map(|b| ((b as f64 / 1073741824.0) * 10.0).round() / 10.0)
                .into();
            v["vram_total"] = read_u64(&g.join("mem_info_vram_total"))
                .map(|b| (b as f64 / 1073741824.0).round())
                .into();
        }
        if let Some(h) = &gpu_hwmon {
            // temp2 = junction (hotspot) on recent AMD cards; temp1 = edge.
            v["gpu_temp"] = read_u64(&h.join("temp1_input")).map(|t| t / 1000).into();
            let power =
                read_u64(&h.join("power1_average")).or_else(|| read_u64(&h.join("power1_input")));
            v["gpu_power"] = power.map(|p| p / 1_000_000).into();
        }
        hub.set("stats", v);
    }
}
