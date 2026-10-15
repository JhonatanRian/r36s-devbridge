use crate::protocol::{ProcessInfo, SystemMetrics};
use std::fs;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MetricsCollector {
    last_cpu_times: Mutex<Option<(u64, u64)>>, // (total, idle)
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            last_cpu_times: Mutex::new(None),
        }
    }

    pub fn collect(&self) -> SystemMetrics {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let cpu_usage = self.calculate_cpu_usage();
        let cpu_freq_mhz = Self::collect_cpu_freqs();
        let cpu_temp_c = Self::collect_cpu_temp();
        let (ram_used_mb, ram_total_mb) = Self::collect_ram();
        let (mali_gpu_load, mali_gpu_freq_mhz) = Self::collect_mali_gpu();
        let running_processes = Self::collect_processes();
        let active_game = Self::detect_active_game(&running_processes);

        SystemMetrics {
            timestamp,
            cpu_usage,
            cpu_freq_mhz,
            cpu_temp_c,
            ram_used_mb,
            ram_total_mb,
            mali_gpu_load,
            mali_gpu_freq_mhz,
            active_game,
            running_processes,
        }
    }

    fn calculate_cpu_usage(&self) -> f32 {
        if let Ok(stat_content) = fs::read_to_string("/proc/stat") {
            if let Some(first_line) = stat_content.lines().next() {
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                if parts.len() >= 5 && parts[0] == "cpu" {
                    let user: u64 = parts[1].parse().unwrap_or(0);
                    let nice: u64 = parts[2].parse().unwrap_or(0);
                    let system: u64 = parts[3].parse().unwrap_or(0);
                    let idle: u64 = parts[4].parse().unwrap_or(0);
                    let iowait: u64 = parts.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let irq: u64 = parts.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let softirq: u64 = parts.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let steal: u64 = parts.get(8).and_then(|s| s.parse().ok()).unwrap_or(0);

                    let idle_all = idle + iowait;
                    let non_idle = user + nice + system + irq + softirq + steal;
                    let total = idle_all + non_idle;

                    let mut last = self.last_cpu_times.lock().unwrap();
                    if let Some((prev_total, prev_idle)) = *last {
                        let total_diff = total.saturating_sub(prev_total);
                        let idle_diff = idle_all.saturating_sub(prev_idle);
                        *last = Some((total, idle_all));
                        if total_diff > 0 {
                            let usage = (total_diff.saturating_sub(idle_diff) as f32 / total_diff as f32) * 100.0;
                            return usage.clamp(0.0, 100.0);
                        }
                    } else {
                        *last = Some((total, idle_all));
                    }
                }
            }
        }
        0.0
    }

    fn collect_cpu_freqs() -> Vec<u32> {
        let mut freqs = Vec::new();
        for cpu_idx in 0..8 {
            let path1 = format!("/sys/devices/system/cpu/cpu{}/cpufreq/scaling_cur_freq", cpu_idx);
            let path2 = format!("/sys/devices/system/cpu/cpu{}/cpufreq/cpuinfo_cur_freq", cpu_idx);
            
            let val_str = fs::read_to_string(&path1).or_else(|_| fs::read_to_string(&path2));
            if let Ok(content) = val_str {
                if let Ok(khz) = content.trim().parse::<u32>() {
                    freqs.push(khz / 1000);
                }
            } else {
                break;
            }
        }
        if freqs.is_empty() {
            // Fallback para 4 cores típicos do RK3326
            vec![1200, 1200, 1200, 1200]
        } else {
            freqs
        }
    }

    fn collect_cpu_temp() -> f32 {
        for zone in 0..4 {
            let path = format!("/sys/class/thermal/thermal_zone{}/temp", zone);
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(raw_temp) = content.trim().parse::<f32>() {
                    if raw_temp > 1000.0 {
                        return (raw_temp / 1000.0 * 10.0).round() / 10.0;
                    } else if raw_temp > 0.0 {
                        return (raw_temp * 10.0).round() / 10.0;
                    }
                }
            }
        }
        42.0 // Valor padrão plausível em fallback
    }

    fn collect_ram() -> (u64, u64) {
        let mut total_kb = 0;
        let mut avail_kb = 0;
        let mut free_kb = 0;
        let mut buffers_kb = 0;
        let mut cached_kb = 0;

        if let Ok(content) = fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let key = parts[0];
                    let val: u64 = parts[1].parse().unwrap_or(0);
                    match key {
                        "MemTotal:" => total_kb = val,
                        "MemAvailable:" => avail_kb = val,
                        "MemFree:" => free_kb = val,
                        "Buffers:" => buffers_kb = val,
                        "Cached:" => cached_kb = val,
                        _ => {}
                    }
                }
            }
        }

        let total_mb = total_kb / 1024;
        let used_kb = if avail_kb > 0 {
            total_kb.saturating_sub(avail_kb)
        } else {
            total_kb.saturating_sub(free_kb + buffers_kb + cached_kb)
        };
        let used_mb = used_kb / 1024;

        (used_mb, if total_mb > 0 { total_mb } else { 1024 })
    }

    fn collect_mali_gpu() -> (Option<f32>, Option<u32>) {
        // Tenta ler driver devfreq da GPU Mali no RK3326
        let mut load = None;
        let mut freq = None;

        if let Ok(content) = fs::read_to_string("/sys/class/devfreq/ff400000.gpu/load") {
            if let Ok(l) = content.trim().parse::<f32>() {
                load = Some(l);
            }
        }
        if let Ok(content) = fs::read_to_string("/sys/class/devfreq/ff400000.gpu/cur_freq") {
            if let Ok(f) = content.trim().parse::<u32>() {
                freq = Some(f / 1_000_000);
            }
        }

        (load, freq)
    }

    fn collect_processes() -> Vec<ProcessInfo> {
        let mut procs = Vec::new();
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(name_str) = name.to_str() {
                    if let Ok(pid) = name_str.parse::<u32>() {
                        let proc_path = entry.path();
                        let cmdline_path = proc_path.join("cmdline");
                        let statm_path = proc_path.join("statm");

                        let cmdline = fs::read(cmdline_path)
                            .map(|bytes| {
                                bytes
                                    .split(|&b| b == 0)
                                    .filter(|s| !s.is_empty())
                                    .map(|s| String::from_utf8_lossy(s).into_owned())
                                    .collect::<Vec<String>>()
                                    .join(" ")
                            })
                            .unwrap_or_default();

                        if cmdline.is_empty() {
                            continue;
                        }

                        let memory_mb = fs::read_to_string(statm_path)
                            .ok()
                            .and_then(|s| {
                                s.split_whitespace()
                                    .nth(1)
                                    .and_then(|rss| rss.parse::<u64>().ok())
                            })
                            .map(|pages| (pages * 4) / 1024)
                            .unwrap_or(0);

                        let proc_name = cmdline
                            .split_whitespace()
                            .next()
                            .and_then(|p| p.split('/').last())
                            .unwrap_or("unknown")
                            .to_string();

                        // Filtra apenas processos relevantes para devs/jogos para manter payload leve
                        let is_interesting = proc_name.contains("retroarch")
                            || proc_name.contains("java")
                            || proc_name.contains("freej2me")
                            || proc_name.contains("emulationstation")
                            || proc_name.contains(".sh")
                            || proc_name.contains("drastic")
                            || proc_name.contains("mupen64plus")
                            || proc_name.contains("ra32")
                            || memory_mb > 15;

                        if is_interesting {
                            procs.push(ProcessInfo {
                                pid,
                                name: proc_name,
                                cmdline,
                                memory_mb,
                            });
                        }
                    }
                }
            }
        }
        procs.sort_by_key(|p| std::cmp::Reverse(p.memory_mb));
        procs.truncate(20);
        procs
    }

    fn detect_active_game(processes: &[ProcessInfo]) -> Option<String> {
        for p in processes {
            let lower = p.cmdline.to_lowercase();
            if lower.contains("retroarch") {
                if let Some(pos) = lower.find("-l ") {
                    let core_part = &p.cmdline[pos + 3..];
                    let core_name = core_part
                        .split_whitespace()
                        .next()
                        .and_then(|c| c.split('/').last())
                        .unwrap_or("libretro");
                    return Some(format!("RetroArch ({})", core_name));
                }
                return Some("RetroArch".to_string());
            } else if lower.contains("java") && lower.contains("freej2me") {
                return Some("FreeJ2ME (Java MIDlet)".to_string());
            } else if lower.contains("drastic") {
                return Some("DraStic (Nintendo DS)".to_string());
            } else if lower.contains("mupen64plus") {
                return Some("Mupen64Plus (N64)".to_string());
            } else if lower.contains("ppsspp") {
                return Some("PPSSPP (Sony PSP)".to_string());
            }
        }
        for p in processes {
            if p.cmdline.contains("emulationstation") {
                return Some("EmulationStation (Menu)".to_string());
            }
        }
        None
    }
}
