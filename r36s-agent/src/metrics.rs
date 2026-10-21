use crate::protocol::{ProcessInfo, SystemMetrics};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct MetricsCollector {
    last_cpu_times: Mutex<Option<(u64, u64)>>, // (total, idle)
    last_cpu_core_times: Mutex<Option<[(u64, u64); 4]>>, // [(total, idle); 4] cpu0..cpu3
    last_proc_times: Mutex<HashMap<u32, (u64, Instant)>>, // pid -> (ticks, instant)
    known_kthreads: Mutex<HashSet<u32>>,
    cached_processes: Mutex<Option<(Vec<ProcessInfo>, Option<String>, Instant)>>,
    stat_file: Mutex<Option<fs::File>>,
    meminfo_file: Mutex<Option<fs::File>>,
    thermal_file: Mutex<Option<fs::File>>,
    fb_info: Mutex<(u16, u16, u8, u8)>,        // (width, height, bpp, format)
    page_size_kb: u64,
    clk_tck: f32,
    num_cpus: f32,
}

impl MetricsCollector {
    pub fn new() -> Self {
        let page_size_kb = unsafe {
            let ps = libc::_SC_PAGESIZE;
            let val = libc::sysconf(ps);
            if val > 0 {
                (val as u64) / 1024
            } else {
                4
            }
        };
        let clk_tck = unsafe {
            let tck = libc::_SC_CLK_TCK;
            let val = libc::sysconf(tck);
            if val > 0 {
                val as f32
            } else {
                100.0
            }
        };
        let num_cpus = unsafe {
            let n = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
            if n > 0 {
                n as f32
            } else {
                4.0
            }
        };

        let stat_file = fs::File::open("/proc/stat").ok();
        let meminfo_file = fs::File::open("/proc/meminfo").ok();
        let mut thermal_file = None;
        for zone in 0..6 {
            let path = format!("/sys/class/thermal/thermal_zone{}/temp", zone);
            if let Ok(file) = fs::File::open(&path) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    let mut test_buf = [0u8; 32];
                    if file.read_at(&mut test_buf, 0).is_ok() {
                        thermal_file = Some(file);
                        break;
                    }
                }
                #[cfg(not(unix))]
                {
                    thermal_file = Some(file);
                    break;
                }
            }
        }

        Self {
            last_cpu_times: Mutex::new(None),
            last_cpu_core_times: Mutex::new(None),
            last_proc_times: Mutex::new(HashMap::new()),
            known_kthreads: Mutex::new(HashSet::new()),
            cached_processes: Mutex::new(None),
            stat_file: Mutex::new(stat_file),
            meminfo_file: Mutex::new(meminfo_file),
            thermal_file: Mutex::new(thermal_file),
            fb_info: Mutex::new((640, 480, 16, 1)),
            page_size_kb,
            clk_tck,
            num_cpus,
        }
    }

    pub fn set_fb_info(&self, width: u16, height: u16, bpp: u8, format: u8) {
        let mut info = self.fb_info.lock().unwrap();
        *info = (width, height, bpp, format);
    }

    pub fn get_fb_info(&self) -> (u16, u16, u8, u8) {
        *self.fb_info.lock().unwrap()
    }

    #[allow(dead_code)]
    pub fn known_kthreads_count(&self) -> usize {
        self.known_kthreads.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn is_known_kthread(&self, pid: u32) -> bool {
        self.known_kthreads.lock().unwrap().contains(&pid)
    }

    #[cfg(test)]
    pub(crate) fn insert_known_kthread_for_test(&self, pid: u32) {
        self.known_kthreads.lock().unwrap().insert(pid);
    }

    pub fn get_cached_or_collect_processes(&self) -> (Vec<ProcessInfo>, Option<String>) {
        self.get_cached_or_collect_processes_with_ttl(std::time::Duration::from_secs(3))
    }

    pub fn get_cached_or_collect_processes_with_ttl(
        &self,
        ttl: std::time::Duration,
    ) -> (Vec<ProcessInfo>, Option<String>) {
        let mut cache = self.cached_processes.lock().unwrap();
        if let Some((ref procs, ref game, last_scan)) = *cache {
            if last_scan.elapsed() < ttl {
                return (procs.clone(), game.clone());
            }
        }
        let procs = self.collect_processes();
        let game = Self::detect_active_game(&procs);
        *cache = Some((procs.clone(), game.clone(), Instant::now()));
        (procs, game)
    }

    #[cfg(test)]
    pub(crate) fn set_cached_processes_for_test(
        &self,
        procs: Vec<ProcessInfo>,
        game: Option<String>,
        last_scan: Instant,
    ) {
        *self.cached_processes.lock().unwrap() = Some((procs, game, last_scan));
    }

    pub fn collect(&self) -> SystemMetrics {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (cpu_usage, cpu_core_usage) = self.calculate_cpu_and_cores_usage();
        let cpu_freq_mhz = Self::collect_cpu_freqs();
        let cpu_temp_c = self.collect_cpu_temp();
        let (ram_used_mb, ram_total_mb, cma_total_mb, cma_free_mb, cma_available) = self.collect_ram_and_cma();
        let (mali_gpu_load, mali_gpu_freq_mhz) = Self::collect_mali_gpu();
        let (running_processes, active_game) = self.get_cached_or_collect_processes();
        let (fb_width, fb_height, fb_bpp, fb_format) = self.get_fb_info();
        let thermal_throttling = Self::evaluate_thermal_throttling(cpu_temp_c, cpu_usage, &cpu_freq_mhz);

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
            cpu_core_usage,
            cma_total_mb,
            cma_free_mb,
            cma_available,
            thermal_throttling,
        }
    }

    fn read_file_at_zero(
        file_mutex: &Mutex<Option<fs::File>>,
        path: &str,
        buf: &mut [u8],
    ) -> Option<usize> {
        let mut guard = file_mutex.lock().unwrap();
        if guard.is_none() {
            *guard = fs::File::open(path).ok();
        }

        if let Some(ref file) = *guard {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileExt;
                if let Ok(n) = file.read_at(buf, 0) {
                    if n > 0 {
                        return Some(n);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                use std::io::{Read, Seek, SeekFrom};
                let mut f = file;
                if f.seek(SeekFrom::Start(0)).is_ok() {
                    if let Ok(n) = f.read(buf) {
                        if n > 0 {
                            return Some(n);
                        }
                    }
                }
            }
        }

        // Se o descritor persistente falhar ou for nulo, tenta reabrir o arquivo de forma resiliente
        if let Ok(new_file) = fs::File::open(path) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileExt;
                if let Ok(n) = new_file.read_at(buf, 0) {
                    *guard = Some(new_file);
                    return Some(n);
                }
            }
            #[cfg(not(unix))]
            {
                use std::io::Read;
                let mut nf = new_file;
                if let Ok(n) = nf.read(buf) {
                    *guard = Some(nf);
                    return Some(n);
                }
            }
            *guard = Some(new_file);
        }
        None
    }

    pub(crate) fn parse_cpu_line(line: &str) -> Option<(&str, u64, u64)> {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        let user: u64 = parts.next()?.parse().ok()?;
        let nice: u64 = parts.next()?.parse().ok()?;
        let system: u64 = parts.next()?.parse().ok()?;
        let idle: u64 = parts.next()?.parse().ok()?;
        let iowait: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let irq: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let softirq: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let steal: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

        let idle_all = idle + iowait;
        let non_idle = user + nice + system + irq + softirq + steal;
        let total = idle_all + non_idle;

        Some((name, total, idle_all))
    }

    pub(crate) fn parse_stat_cpus(content: &str) -> (Option<(u64, u64)>, Option<[(u64, u64); 4]>) {
        let ends_with_nl = content.ends_with('\n');
        let mut lines = content.lines().peekable();
        let mut global_cpu = None;
        let mut core_map: [Option<(u64, u64)>; 4] = [None, None, None, None];

        while let Some(line) = lines.next() {
            let is_last = lines.peek().is_none();
            if is_last && !ends_with_nl {
                // Buffer truncado no meio da linha
                break;
            }

            if let Some((name, total, idle)) = Self::parse_cpu_line(line) {
                if name == "cpu" {
                    global_cpu = Some((total, idle));
                } else if let Some(idx_str) = name.strip_prefix("cpu") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < 4 {
                            core_map[idx] = Some((total, idle));
                        }
                    }
                }
            }
        }

        let cores = match core_map {
            [Some(c0), Some(c1), Some(c2), Some(c3)] => Some([c0, c1, c2, c3]),
            _ => None,
        };

        (global_cpu, cores)
    }

    #[allow(dead_code)]
    pub(crate) fn parse_cpu_stat(content: &str) -> Option<(u64, u64)> {
        Self::parse_stat_cpus(content).0
    }

    pub(crate) fn calculate_cores_usage_from_samples(
        last_cores: &mut Option<[(u64, u64); 4]>,
        curr_cores: Option<[(u64, u64); 4]>,
    ) -> Vec<f32> {
        let mut core_usages = Vec::new();
        if let Some(curr) = curr_cores {
            if let Some(prev) = *last_cores {
                let mut all_valid = true;
                let mut usages = Vec::with_capacity(4);
                for i in 0..4 {
                    let (curr_total, curr_idle) = curr[i];
                    let (prev_total, prev_idle) = prev[i];
                    let total_diff = curr_total.saturating_sub(prev_total);
                    let idle_diff = curr_idle.saturating_sub(prev_idle);
                    if total_diff == 0 {
                        all_valid = false;
                        break;
                    }
                    let usage = ((total_diff.saturating_sub(idle_diff) as f32
                        / total_diff as f32)
                        * 100.0)
                        .clamp(0.0, 100.0);
                    usages.push((usage * 10.0).round() / 10.0);
                }
                if all_valid && usages.len() == 4 {
                    core_usages = usages;
                }
            }
            *last_cores = Some(curr);
        } else {
            *last_cores = None;
        }
        core_usages
    }

    fn calculate_cpu_and_cores_usage(&self) -> (f32, Vec<f32>) {
        let mut buf = [0u8; 1024];
        let mut global_usage = 0.0;
        let mut core_usages = Vec::new();

        if let Some(n) = Self::read_file_at_zero(&self.stat_file, "/proc/stat", &mut buf) {
            if let Ok(stat_content) = std::str::from_utf8(&buf[..n]) {
                let (global_cpu, cores) = Self::parse_stat_cpus(stat_content);

                if let Some((total, idle_all)) = global_cpu {
                    let mut last = self.last_cpu_times.lock().unwrap();
                    if let Some((prev_total, prev_idle)) = *last {
                        let total_diff = total.saturating_sub(prev_total);
                        let idle_diff = idle_all.saturating_sub(prev_idle);
                        *last = Some((total, idle_all));
                        if total_diff > 0 {
                            let usage = (total_diff.saturating_sub(idle_diff) as f32
                                / total_diff as f32)
                                * 100.0;
                            global_usage = usage.clamp(0.0, 100.0);
                        }
                    } else {
                        *last = Some((total, idle_all));
                    }
                }

                let mut last_cores = self.last_cpu_core_times.lock().unwrap();
                core_usages = Self::calculate_cores_usage_from_samples(&mut last_cores, cores);
            }
        }

        (global_usage, core_usages)
    }

    #[allow(dead_code)]
    fn calculate_cpu_usage(&self) -> f32 {
        self.calculate_cpu_and_cores_usage().0
    }

    fn collect_cpu_freqs() -> Vec<u32> {
        let mut freqs = Vec::new();
        for cpu_idx in 0..8 {
            let path1 = format!(
                "/sys/devices/system/cpu/cpu{}/cpufreq/scaling_cur_freq",
                cpu_idx
            );
            let path2 = format!(
                "/sys/devices/system/cpu/cpu{}/cpufreq/cpuinfo_cur_freq",
                cpu_idx
            );

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

    pub(crate) fn parse_cpu_temp(content: &str) -> Option<f32> {
        if let Ok(raw_temp) = content.trim().parse::<f32>() {
            if raw_temp > 1000.0 {
                return Some((raw_temp / 1000.0 * 10.0).round() / 10.0);
            } else if raw_temp > 0.0 {
                return Some((raw_temp * 10.0).round() / 10.0);
            }
        }
        None
    }

    fn collect_cpu_temp(&self) -> Option<f32> {
        let mut buf = [0u8; 64];

        // 1. Tenta ler pelo handle persistente existente via pread
        {
            let guard = self.thermal_file.lock().unwrap();
            if let Some(ref file) = *guard {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    if let Ok(n) = file.read_at(&mut buf, 0) {
                        if n > 0 {
                            if let Ok(content) = std::str::from_utf8(&buf[..n]) {
                                if let Some(temp) = Self::parse_cpu_temp(content) {
                                    return Some(temp);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Se falhar ou não tiver handle, busca zona térmica funcional e persiste handle
        for zone in 0..6 {
            let path = format!("/sys/class/thermal/thermal_zone{}/temp", zone);
            if let Ok(file) = fs::File::open(&path) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    if let Ok(n) = file.read_at(&mut buf, 0) {
                        if n > 0 {
                            if let Ok(content) = std::str::from_utf8(&buf[..n]) {
                                if let Some(temp) = Self::parse_cpu_temp(content) {
                                    *self.thermal_file.lock().unwrap() = Some(file);
                                    return Some(temp);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    pub(crate) fn evaluate_thermal_throttling(
        temp_c: Option<f32>,
        cpu_usage: f32,
        freqs_mhz: &[u32],
    ) -> Option<bool> {
        if let Some(temp) = temp_c {
            // Limiar crítico configurado: >= 80 °C indica forte suspeita de throttling térmico
            if temp >= 80.0 {
                return Some(true);
            }

            // Queda de frequência sob carga pesada e temperatura elevada (>= 75 °C)
            // No RK3326 a frequência máxima é ~1.3-1.5 GHz. Se sob carga pesada (>= 80%)
            // as frequências caírem para patamares baixos (< 1000 MHz) com temperatura elevada:
            if temp >= 75.0 && cpu_usage >= 80.0 && !freqs_mhz.is_empty() && freqs_mhz.iter().all(|&f| f > 0 && f < 1000) {
                return Some(true);
            }

            // Temperatura monitorada normalmente abaixo de 80 °C sem evidência de limitação
            return Some(false);
        }

        None
    }

    pub(crate) fn parse_meminfo_ext(content: &str) -> (u64, u64, u64, u64, bool) {
        let ends_with_nl = content.ends_with('\n');
        let mut lines = content.lines().peekable();

        let mut total_kb: u64 = 0;
        let mut avail_kb: u64 = 0;
        let mut free_kb: u64 = 0;
        let mut buffers_kb: u64 = 0;
        let mut cached_kb: u64 = 0;

        let mut cma_total_kb: Option<u64> = None;
        let mut cma_free_kb: Option<u64> = None;

        while let Some(line) = lines.next() {
            let is_last = lines.peek().is_none();
            if is_last && !ends_with_nl {
                // Buffer truncado no meio da linha
                break;
            }

            let mut parts = line.split_whitespace();
            if let (Some(key), Some(val_str)) = (parts.next(), parts.next()) {
                match key {
                    "MemTotal:" => {
                        if let Ok(v) = val_str.parse() {
                            total_kb = v;
                        }
                    }
                    "MemAvailable:" => {
                        if let Ok(v) = val_str.parse() {
                            avail_kb = v;
                        }
                    }
                    "MemFree:" => {
                        if let Ok(v) = val_str.parse() {
                            free_kb = v;
                        }
                    }
                    "Buffers:" => {
                        if let Ok(v) = val_str.parse() {
                            buffers_kb = v;
                        }
                    }
                    "Cached:" => {
                        if let Ok(v) = val_str.parse() {
                            cached_kb = v;
                        }
                    }
                    "CmaTotal:" => {
                        if let Ok(v) = val_str.parse() {
                            cma_total_kb = Some(v);
                        }
                    }
                    "CmaFree:" => {
                        if let Ok(v) = val_str.parse() {
                            cma_free_kb = Some(v);
                        }
                    }
                    _ => {}
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

        let (cma_total_mb, cma_free_mb, cma_available) = match (cma_total_kb, cma_free_kb) {
            (Some(total), Some(free)) if free <= total => {
                (total / 1024, free / 1024, true)
            }
            _ => (0, 0, false),
        };

        (used_mb, total_mb, cma_total_mb, cma_free_mb, cma_available)
    }

    #[allow(dead_code)]
    pub(crate) fn parse_meminfo(content: &str) -> (u64, u64) {
        let (used_mb, total_mb, _, _, _) = Self::parse_meminfo_ext(content);
        (used_mb, total_mb)
    }

    fn collect_ram_and_cma(&self) -> (u64, u64, u64, u64, bool) {
        let mut buf = [0u8; 2048];
        if let Some(n) = Self::read_file_at_zero(&self.meminfo_file, "/proc/meminfo", &mut buf) {
            if let Ok(content) = std::str::from_utf8(&buf[..n]) {
                return Self::parse_meminfo_ext(content);
            }
        }
        (0, 0, 0, 0, false)
    }

    #[allow(dead_code)]
    fn collect_ram(&self) -> (u64, u64) {
        let (used, total, _, _, _) = self.collect_ram_and_cma();
        (used, total)
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

    pub fn collect_processes(&self) -> Vec<ProcessInfo> {
        let mut procs = Vec::new();
        let now = Instant::now();
        let mut new_proc_times = HashMap::new();
        let mut last_proc_times = self.last_proc_times.lock().unwrap();
        let mut known_kthreads = self.known_kthreads.lock().unwrap();
        let mut seen_pids = HashSet::new();

        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Ok(pid) = name.to_string_lossy().parse::<u32>() {
                    seen_pids.insert(pid);

                    // 1. If known kernel thread, skip immediately without any VFS reads
                    if known_kthreads.contains(&pid) {
                        continue;
                    }

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
                            // Empty cmdline is a kernel thread; cache PID and skip
                            known_kthreads.insert(pid);
                            continue;
                        }

                        let page_size_kb = self.page_size_kb;

                        let memory_mb = fs::read_to_string(statm_path)
                            .ok()
                            .and_then(|s| {
                                s.split_whitespace()
                                    .nth(1)
                                    .and_then(|rss| rss.parse::<u64>().ok())
                            })
                            .map(|pages| (pages * page_size_kb) / 1024)
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
                                let stat_parts: Vec<&str> =
                                    after_paren.split_whitespace().collect();
                                // Campo 14 utime = índice 11 após parêntese; Campo 15 stime = índice 12
                                if stat_parts.len() >= 13 {
                                    let utime: u64 = stat_parts[11].parse().unwrap_or(0);
                                    let stime: u64 = stat_parts[12].parse().unwrap_or(0);
                                    let total_ticks = utime + stime;
                                    new_proc_times.insert(pid, (total_ticks, now));

                                    if let Some(&(prev_ticks, prev_time)) =
                                        last_proc_times.get(&pid)
                                    {
                                        let tick_diff = total_ticks.saturating_sub(prev_ticks);
                                        let time_diff = now.duration_since(prev_time).as_secs_f32();
                                        cpu_percent = Self::calculate_process_cpu_percent(
                                            tick_diff,
                                            time_diff,
                                            self.clk_tck,
                                            self.num_cpus,
                                        );
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

        // Cache maintenance: prune stale kthreads that no longer exist in /proc
        known_kthreads.retain(|pid| seen_pids.contains(pid));
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

    pub fn detect_active_game(processes: &[ProcessInfo]) -> Option<String> {
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

    /// Calcula a porcentagem de CPU utilizada por um processo normalizada pela quantidade de núcleos (0%..100%).
    /// Na escala de sistema total (Solaris mode), se 1 núcleo estiver 100% ocupado em um SoC quad-core (como RK3326),
    /// a função retornará 25.0%, harmonizando com o medidor de CPU global.
    pub fn calculate_process_cpu_percent(
        tick_diff: u64,
        time_diff: f32,
        clk_tck: f32,
        num_cpus: f32,
    ) -> f32 {
        if time_diff <= 0.05 || clk_tck <= 0.0 || num_cpus <= 0.0 {
            return 0.0;
        }
        let raw_cpu = ((tick_diff as f32 / clk_tck) / time_diff) * 100.0;
        (raw_cpu / num_cpus).clamp(0.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_kthreads_cache_and_pruning() {
        let collector = MetricsCollector::new();
        assert_eq!(collector.known_kthreads_count(), 0);

        // Insert a non-existent PID to test cache maintenance / pruning
        let dead_pid = 9_999_999;
        collector.insert_known_kthread_for_test(dead_pid);
        assert!(collector.is_known_kthread(dead_pid));

        let _ = collector.collect_processes();

        // Dead PID must be pruned because it doesn't exist in /proc
        assert!(!collector.is_known_kthread(dead_pid));

        // If running on Linux, kernel threads should have been discovered and cached
        #[cfg(target_os = "linux")]
        {
            // On a real Linux system with /proc, there are always kthreads (e.g. kthreadd with pid 2)
            if std::path::Path::new("/proc/2/cmdline").exists() {
                let cmdline = std::fs::read("/proc/2/cmdline").unwrap_or_default();
                if cmdline.is_empty() {
                    assert!(collector.is_known_kthread(2), "PID 2 (kthreadd) should be identified as kthread");
                }
            }
        }
    }

    #[test]
    fn test_detect_active_game_patterns() {
        let make_proc = |name: &str, cmdline: &str| ProcessInfo {
            pid: 1000,
            name: name.to_string(),
            cmdline: cmdline.to_string(),
            memory_mb: 50,
            cpu_percent: 15.0,
        };

        // RetroArch with core
        let procs = vec![make_proc("retroarch", "/usr/bin/retroarch -l /cores/snes9x_libretro.so /roms/game.sfc")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("RetroArch (snes9x_libretro.so)".to_string()));

        // RetroArch without core
        let procs = vec![make_proc("retroarch", "/usr/bin/retroarch /roms/game.sfc")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("RetroArch".to_string()));

        // FreeJ2ME
        let procs = vec![make_proc("java", "java -jar /opt/freej2me/freej2me.jar")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("FreeJ2ME (Java)".to_string()));

        // DraStic
        let procs = vec![make_proc("drastic", "/opt/drastic/drastic /roms/nds/game.nds")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("DraStic (NDS)".to_string()));

        // Mupen64Plus
        let procs = vec![make_proc("mupen64plus", "/opt/mupen64plus/mupen64plus /roms/n64/game.z64")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("Mupen64Plus (N64)".to_string()));

        // PPSSPP
        let procs = vec![make_proc("ppsspp", "/opt/ppsspp/PPSSPPSDL /roms/psp/game.iso")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("PPSSPP (PSP)".to_string()));

        // Port script
        let procs = vec![make_proc("2048.sh", "/roms/ports/2048.sh")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), Some("Port: 2048.sh".to_string()));

        // None
        let procs = vec![make_proc("bash", "/bin/bash")];
        assert_eq!(MetricsCollector::detect_active_game(&procs), None);
    }

    #[test]
    fn test_process_cpu_percent_normalized_to_core_count() {
        let clk_tck = 100.0;
        let time_diff = 1.0; // 1 second
        let num_cpus = 4.0;  // 4 cores on RK3326

        // 100 ticks in 1s on clk_tck=100 means 100% of 1 core
        // Normalized by 4 cores, this should equal 25.0%
        let cpu_one_core = MetricsCollector::calculate_process_cpu_percent(100, time_diff, clk_tck, num_cpus);
        assert!((cpu_one_core - 25.0).abs() < 0.01, "Expected ~25.0%, got {}", cpu_one_core);

        // 400 ticks in 1s means all 4 cores saturated (400% in Irix mode)
        // Normalized by 4 cores, this should equal 100.0%
        let cpu_all_cores = MetricsCollector::calculate_process_cpu_percent(400, time_diff, clk_tck, num_cpus);
        assert!((cpu_all_cores - 100.0).abs() < 0.01, "Expected 100.0%, got {}", cpu_all_cores);

        // Clamping to 100.0% max
        let cpu_clamped = MetricsCollector::calculate_process_cpu_percent(800, time_diff, clk_tck, num_cpus);
        assert_eq!(cpu_clamped, 100.0);

        // Handling edge cases (zero time, zero clk_tck, zero cpus)
        assert_eq!(MetricsCollector::calculate_process_cpu_percent(100, 0.01, clk_tck, num_cpus), 0.0);
        assert_eq!(MetricsCollector::calculate_process_cpu_percent(100, time_diff, 0.0, num_cpus), 0.0);
        assert_eq!(MetricsCollector::calculate_process_cpu_percent(100, time_diff, clk_tck, 0.0), 0.0);
    }

    #[test]
    fn test_process_cache_validity_window() {
        let collector = MetricsCollector::new();

        let fake_proc = ProcessInfo {
            pid: 7777,
            name: "mock_game".to_string(),
            cmdline: "/opt/drastic/drastic".to_string(),
            memory_mb: 42,
            cpu_percent: 25.0,
        };

        // Seed cache with mock data and recent timestamp
        let now = Instant::now();
        collector.set_cached_processes_for_test(
            vec![fake_proc.clone()],
            Some("DraStic (NDS)".to_string()),
            now,
        );

        // Within 3 seconds: should return cached data directly
        let (cached_procs, cached_game) = collector.get_cached_or_collect_processes();
        assert_eq!(cached_game, Some("DraStic (NDS)".to_string()));
        assert_eq!(cached_procs.len(), 1);
        assert_eq!(cached_procs[0].pid, 7777);

        // Seed cache with expired timestamp (> 3 seconds ago)
        let expired_time = now - std::time::Duration::from_secs(4);
        collector.set_cached_processes_for_test(
            vec![fake_proc],
            Some("DraStic (NDS)".to_string()),
            expired_time,
        );

        // After expiration: should re-scan processes and update cache
        let (fresh_procs, fresh_game) = collector.get_cached_or_collect_processes();
        // Since PID 7777 doesn't exist on host, fresh scan won't have it
        assert!(fresh_procs.iter().all(|p| p.pid != 7777));
        assert_ne!(fresh_game, Some("DraStic (NDS)".to_string()));
    }

    #[test]
    fn test_parse_cpu_stat_accuracy() {
        // Sample /proc/stat CPU line
        let stat_sample = "cpu  100 20 50 800 30 10 5 2 0 0\ncpu0 50 10 ...\n";
        let parsed = MetricsCollector::parse_cpu_stat(stat_sample);
        assert!(parsed.is_some());
        let (total, idle_all) = parsed.unwrap();
        // idle_all = idle (800) + iowait (30) = 830
        assert_eq!(idle_all, 830);
        // non_idle = user (100) + nice (20) + system (50) + irq (10) + softirq (5) + steal (2) = 187
        // total = 830 + 187 = 1017
        assert_eq!(total, 1017);

        // Invalid line without cpu prefix
        assert_eq!(MetricsCollector::parse_cpu_stat("intr 1234567"), None);
        // Empty content
        assert_eq!(MetricsCollector::parse_cpu_stat(""), None);
    }

    #[test]
    fn test_parse_meminfo_accuracy() {
        // Modern kernel with MemAvailable
        let meminfo_available = "MemTotal:       1024000 kB\n\
MemFree:         200000 kB\n\
MemAvailable:    600000 kB\n\
Buffers:          50000 kB\n\
Cached:          150000 kB\n";

        let (used_mb, total_mb) = MetricsCollector::parse_meminfo(meminfo_available);
        assert_eq!(total_mb, 1000); // 1024000 / 1024
        assert_eq!(used_mb, 414);  // (1024000 - 600000) / 1024 = 424000 / 1024 = 414

        // Older kernel fallback (without MemAvailable)
        let meminfo_fallback = "MemTotal:       1024000 kB\n\
MemFree:         200000 kB\n\
Buffers:          50000 kB\n\
Cached:          150000 kB\n";

        let (used_mb2, total_mb2) = MetricsCollector::parse_meminfo(meminfo_fallback);
        assert_eq!(total_mb2, 1000);
        // used = 1024000 - (200000 + 50000 + 150000) = 624000 / 1024 = 609
        assert_eq!(used_mb2, 609);
    }

    #[test]
    fn test_parse_cma_accuracy() {
        // 1. Válido e conversão KiB -> MiB
        let meminfo_valid = "MemTotal:       1024000 kB\n\
MemFree:         200000 kB\n\
CmaTotal:         32768 kB\n\
CmaFree:           8192 kB\n";
        let (_, _, cma_total, cma_free, cma_avail) = MetricsCollector::parse_meminfo_ext(meminfo_valid);
        assert_eq!(cma_total, 32); // 32768 / 1024
        assert_eq!(cma_free, 8);   // 8192 / 1024
        assert!(cma_avail);

        // 2. Ausência total de campos CMA
        let meminfo_no_cma = "MemTotal:       1024000 kB\nMemFree: 200000 kB\n";
        let (_, _, cma_total0, cma_free0, cma_avail0) = MetricsCollector::parse_meminfo_ext(meminfo_no_cma);
        assert_eq!(cma_total0, 0);
        assert_eq!(cma_free0, 0);
        assert!(!cma_avail0);

        // 3. Apenas CmaTotal presente (CmaFree ausente)
        let meminfo_partial1 = "MemTotal: 1024000 kB\nCmaTotal: 16384 kB\n";
        let (_, _, _, _, cma_avail1) = MetricsCollector::parse_meminfo_ext(meminfo_partial1);
        assert!(!cma_avail1);

        // 4. Apenas CmaFree presente (CmaTotal ausente)
        let meminfo_partial2 = "MemTotal: 1024000 kB\nCmaFree: 8192 kB\n";
        let (_, _, _, _, cma_avail2) = MetricsCollector::parse_meminfo_ext(meminfo_partial2);
        assert!(!cma_avail2);

        // 5. Incoerência: free > total
        let meminfo_incoherent = "MemTotal: 1024000 kB\nCmaTotal: 8192 kB\nCmaFree: 16384 kB\n";
        let (_, _, cma_total_inc, cma_free_inc, cma_avail_inc) = MetricsCollector::parse_meminfo_ext(meminfo_incoherent);
        assert_eq!(cma_total_inc, 0);
        assert_eq!(cma_free_inc, 0);
        assert!(!cma_avail_inc);

        // 6. Truncamento sem newline final
        let meminfo_truncated = "MemTotal: 1024000 kB\nCmaTotal: 16384 kB\nCmaFree: 12"; // sem \n
        let (_, _, _, _, cma_avail_trunc) = MetricsCollector::parse_meminfo_ext(meminfo_truncated);
        assert!(!cma_avail_trunc);

        // 7. Campo malformado
        let meminfo_malformed = "MemTotal: 1024000 kB\nCmaTotal: 16384 kB\nCmaFree: not_a_number kB\n";
        let (_, _, _, _, cma_avail_mal) = MetricsCollector::parse_meminfo_ext(meminfo_malformed);
        assert!(!cma_avail_mal);
    }

    #[test]
    fn test_parse_cpu_temp() {
        // Millidegrees (typical on Linux thermal sysfs, e.g. 48500 = 48.5C)
        assert_eq!(MetricsCollector::parse_cpu_temp("48500\n"), Some(48.5));
        // Direct Celsius (e.g. 52.3)
        assert_eq!(MetricsCollector::parse_cpu_temp("52.3\n"), Some(52.3));
        // Zero or negative
        assert_eq!(MetricsCollector::parse_cpu_temp("0\n"), None);
        // Invalid non-numeric
        assert_eq!(MetricsCollector::parse_cpu_temp("invalid"), None);
    }

    #[test]
    fn test_evaluate_thermal_throttling() {
        // 1. Estado Some(true): temperatura alcança ou ultrapassa 80 °C
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(Some(80.0), 30.0, &[1200]),
            Some(true)
        );
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(Some(85.5), 95.0, &[600, 600, 600, 600]),
            Some(true)
        );

        // 2. Estado Some(true): queda de frequência sob carga (>= 80%) e temperatura elevada (>= 75 °C)
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(Some(76.5), 88.0, &[816, 816, 816, 816]),
            Some(true)
        );

        // 3. Estado Some(false): clock baixo em repouso/idle NÃO deve acusar throttling
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(Some(76.0), 15.0, &[600, 600, 600, 600]),
            Some(false)
        );

        // 4. Estado Some(false): temperatura elevada mas frequências normais máximas
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(Some(76.0), 90.0, &[1200, 1200, 1200, 1200]),
            Some(false)
        );

        // 5. Estado Some(false): operação normal fria
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(Some(55.0), 50.0, &[1200, 1200]),
            Some(false)
        );

        // 6. Estado None: sem evidência térmica (sensor indisponível)
        assert_eq!(
            MetricsCollector::evaluate_thermal_throttling(None, 90.0, &[600, 600]),
            None
        );
    }

    #[test]
    fn test_parse_stat_cpus_and_order() {
        // Out-of-order cpu cores in /proc/stat
        let sample = "cpu  400 0 100 500 0 0 0 0 0 0\n\
cpu2 200 0 50 250 0 0 0 0 0 0\n\
cpu0 100 0 50 350 0 0 0 0 0 0\n\
cpu3 300 0 100 100 0 0 0 0 0 0\n\
cpu1 150 0 50 300 0 0 0 0 0 0\n\
intr 987654\n";

        let (global, cores) = MetricsCollector::parse_stat_cpus(sample);
        assert_eq!(global, Some((1000, 500))); // total: 1000, idle: 500
        assert!(cores.is_some());
        let c = cores.unwrap();
        // cpu0: non_idle = 150, idle = 350, total = 500
        assert_eq!(c[0], (500, 350));
        // cpu1: non_idle = 200, idle = 300, total = 500
        assert_eq!(c[1], (500, 300));
        // cpu2: non_idle = 250, idle = 250, total = 500
        assert_eq!(c[2], (500, 250));
        // cpu3: non_idle = 400, idle = 100, total = 500
        assert_eq!(c[3], (500, 100));
    }

    #[test]
    fn test_parse_stat_cpus_truncated_and_missing() {
        // Missing cpu3
        let sample_missing = "cpu  100 0 0 100\n\
cpu0 25 0 0 25\n\
cpu1 25 0 0 25\n\
cpu2 25 0 0 25\n";
        let (_, cores) = MetricsCollector::parse_stat_cpus(sample_missing);
        assert_eq!(cores, None, "Amostra sem cpu3 deve ser None");

        // Truncated line without trailing newline
        let sample_truncated = "cpu  100 0 0 100\n\
cpu0 25 0 0 25\n\
cpu1 25 0 0 25\n\
cpu2 25 0 0 25\n\
cpu3 25 0"; // no newline, cut off
        let (_, cores2) = MetricsCollector::parse_stat_cpus(sample_truncated);
        assert_eq!(cores2, None, "Amostra truncada sem newline deve ser descartada");

        // Malformed line
        let sample_malformed = "cpu  100 0 0 100\n\
cpu0 25 0 0 25\n\
cpu1 25 0 0 25\n\
cpu2 25 0 0 25\n\
cpu3 25 invalid 0 25\n";
        let (_, cores3) = MetricsCollector::parse_stat_cpus(sample_malformed);
        assert_eq!(cores3, None, "Amostra com dados inválidos deve ser None");
    }

    #[test]
    fn test_calculate_cores_usage_deltas_delta_zero_and_limits() {
        let mut last_cores = None;

        let sample1 = Some([
            (1000, 800), // cpu0
            (1000, 700), // cpu1
            (1000, 600), // cpu2
            (1000, 500), // cpu3
        ]);

        // Primeira amostra: não há delta anterior, deve retornar vazio
        let usage1 = MetricsCollector::calculate_cores_usage_from_samples(&mut last_cores, sample1);
        assert!(usage1.is_empty(), "Primeira amostra deve ser vazia");

        let sample2 = Some([
            (1100, 850), // diff: total 100, idle 50 -> non_idle 50 -> 50.0%
            (1100, 720), // diff: total 100, idle 20 -> non_idle 80 -> 80.0%
            (1100, 690), // diff: total 100, idle 90 -> non_idle 10 -> 10.0%
            (1100, 500), // diff: total 100, idle 0  -> non_idle 100 -> 100.0%
        ]);

        // Segunda amostra: 4 cores calculados na ordem
        let usage2 = MetricsCollector::calculate_cores_usage_from_samples(&mut last_cores, sample2);
        assert_eq!(usage2, vec![50.0, 80.0, 10.0, 100.0]);

        // Terceira amostra: delta zero (valores idênticos à amostra 2)
        let usage3 = MetricsCollector::calculate_cores_usage_from_samples(&mut last_cores, sample2);
        assert!(usage3.is_empty(), "Delta zero deve retornar vazio sem fabricar 0%");

        // Amostra com wrap / total < idle ou valores extremos: clamp 0..100
        let sample_prev = Some([
            (1000, 1000),
            (1000, 1000),
            (1000, 1000),
            (1000, 1000),
        ]);
        let mut last_cores_clamp = sample_prev;
        let sample_clamp = Some([
            (1100, 1200), // idle_diff maior que total_diff (underflow/wrap)
            (1100, 900),  // total 100, idle 0 -> 100%
            (1100, 1100), // total 100, idle 100 -> 0%
            (1100, 1050), // total 100, idle 50 -> 50%
        ]);
        let usage_clamp = MetricsCollector::calculate_cores_usage_from_samples(&mut last_cores_clamp, sample_clamp);
        assert_eq!(usage_clamp, vec![0.0, 100.0, 0.0, 50.0]);

        // Amostra incompleta após amostra válida reseta last_cores
        let usage_none = MetricsCollector::calculate_cores_usage_from_samples(&mut last_cores_clamp, None);
        assert!(usage_none.is_empty());
        assert_eq!(last_cores_clamp, None);
    }

    #[test]
    fn test_collector_vital_metrics_integration() {
        let collector = MetricsCollector::new();
        let metrics = collector.collect();
        #[cfg(target_os = "linux")]
        {
            assert!(metrics.ram_total_mb > 0, "RAM total deve ser maior que zero no Linux");
            assert!(metrics.ram_used_mb <= metrics.ram_total_mb, "RAM usada não deve exceder RAM total");
            assert!(metrics.cpu_usage >= 0.0 && metrics.cpu_usage <= 100.0, "Uso de CPU deve estar entre 0 e 100%");
        }
    }
}

