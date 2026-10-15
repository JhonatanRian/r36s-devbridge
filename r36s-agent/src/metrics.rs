use crate::protocol::{ProcessInfo, SystemMetrics};
use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct MetricsCollector {
    last_cpu_times: Mutex<Option<(u64, u64)>>, // (total, idle)
    last_proc_times: Mutex<HashMap<u32, (u64, Instant)>>, // pid -> (ticks, instant)
    fb_info: Mutex<(u16, u16, u8, u8)>, // (width, height, bpp, format)
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            last_cpu_times: Mutex::new(None),
            last_proc_times: Mutex::new(HashMap::new()),
            fb_info: Mutex::new((640, 480, 16, 1)),
        }
    }

    pub fn set_fb_info(&self, width: u16, height: u16, bpp: u8, format: u8) {
        let mut info = self.fb_info.lock().unwrap();
        *info = (width, height, bpp, format);
    }

    pub fn get_fb_info(&self) -> (u16, u16, u8, u8) {
        *self.fb_info.lock().unwrap()
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
        let running_processes = self.collect_processes();
        let active_game = Self::detect_active_game(&running_processes);
        let (fb_width, fb_height, fb_bpp, fb_format) = self.get_fb_info();

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
            fb_width,
            fb_height,
            fb_bpp,
            fb_format,
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
        freqs
    }

    fn collect_cpu_temp() -> Option<f32> {
        for zone in 0..6 {
            let path = format!("/sys/class/thermal/thermal_zone{}/temp", zone);
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(raw_temp) = content.trim().parse::<f32>() {
                    if raw_temp > 1000.0 {
                        return Some((raw_temp / 1000.0 * 10.0).round() / 10.0);
                    } else if raw_temp > 0.0 {
                        return Some((raw_temp * 10.0).round() / 10.0);
                    }
                }
            }
        }
        None
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

        (used_mb, total_mb)
    }

    fn collect_mali_gpu() -> (Option<f32>, Option<u32>) {
        let mut load = None;
        let mut freq = None;

        for prefix in &["ff400000.gpu", "fde60000.gpu", "gpu"] {
            let base = format!("/sys/devices/platform/{}", prefix);
            if !std::path::Path::new(&base).exists() {
                continue;
            }

            let util_path = format!("{}/utilisation", base);
            if let Ok(content) = fs::read_to_string(&util_path) {
                if let Ok(val) = content.trim().parse::<f32>() {
                    load = Some(val.clamp(0.0, 100.0));
                }
            }

            let freq_path = format!("{}/devfreq/{}/cur_freq", base, prefix);
            if let Ok(content) = fs::read_to_string(&freq_path) {
                if let Ok(val) = content.trim().parse::<u32>() {
                    freq = Some(val / 1_000_000);
                }
            }
            break;
        }

        (load, freq)
    }

    fn collect_processes(&self) -> Vec<ProcessInfo> {
        let mut procs = Vec::new();
        let now = Instant::now();
        let mut new_proc_times = HashMap::new();
        let mut last_proc_times = self.last_proc_times.lock().unwrap();

        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Ok(pid) = name.to_string_lossy().parse::<u32>() {
                    let cmdline_path = entry.path().join("cmdline");
                    let statm_path = entry.path().join("statm");
                    let stat_path = entry.path().join("stat");

                    if let Ok(cmd_bytes) = fs::read(cmdline_path) {
                        let cmdline = cmd_bytes
                            .split(|&b| b == 0)
                            .filter(|s| !s.is_empty())
                            .map(|s| String::from_utf8_lossy(s).into_owned())
                            .collect::<Vec<String>>()
                            .join(" ");

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

                        // Calcula CPU% por processo lendo utime + stime de /proc/[pid]/stat
                        let mut cpu_percent = 0.0f32;
                        if let Ok(stat_content) = fs::read_to_string(stat_path) {
                            if let Some(rparen_pos) = stat_content.rfind(')') {
                                let after_paren = &stat_content[rparen_pos + 1..];
                                let stat_parts: Vec<&str> = after_paren.split_whitespace().collect();
                                // Campo 14 utime = índice 11 após parêntese; Campo 15 stime = índice 12
                                if stat_parts.len() >= 13 {
                                    let utime: u64 = stat_parts[11].parse().unwrap_or(0);
                                    let stime: u64 = stat_parts[12].parse().unwrap_or(0);
                                    let total_ticks = utime + stime;
                                    new_proc_times.insert(pid, (total_ticks, now));

                                    if let Some(&(prev_ticks, prev_time)) = last_proc_times.get(&pid) {
                                        let tick_diff = total_ticks.saturating_sub(prev_ticks);
                                        let time_diff = now.duration_since(prev_time).as_secs_f32();
                                        if time_diff > 0.05 {
                                            // 100 ticks por segundo no Linux ARM padrão
                                            cpu_percent = ((tick_diff as f32 / 100.0) / time_diff) * 100.0;
                                            cpu_percent = cpu_percent.clamp(0.0, 400.0);
                                        }
                                    }
                                }
                            }
                        }

                        let is_interesting = proc_name.contains("retroarch")
                            || proc_name.contains("java")
                            || proc_name.contains("freej2me")
                            || proc_name.contains("emulationstation")
                            || proc_name.contains(".sh")
                            || proc_name.contains("drastic")
                            || proc_name.contains("mupen64plus")
                            || proc_name.contains("ra32")
                            || cpu_percent > 1.0
                            || memory_mb > 15;

                        if is_interesting {
                            procs.push(ProcessInfo {
                                pid,
                                name: proc_name,
                                cmdline,
                                memory_mb,
                                cpu_percent: (cpu_percent * 10.0).round() / 10.0,
                            });
                        }
                    }
                }
            }
        }
        *last_proc_times = new_proc_times;

        procs.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.memory_mb.cmp(&a.memory_mb))
        });
        procs.truncate(30);
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
                return Some("FreeJ2ME (Java)".to_string());
            } else if lower.contains("drastic") {
                return Some("DraStic (NDS)".to_string());
            } else if lower.contains("mupen64plus") {
                return Some("Mupen64Plus (N64)".to_string());
            } else if lower.contains("ppsspp") {
                return Some("PPSSPP (PSP)".to_string());
            } else if p.name.ends_with(".sh") && !p.cmdline.contains("r36s-agent") {
                return Some(format!("Port: {}", p.name));
            }
        }
        None
    }
}
