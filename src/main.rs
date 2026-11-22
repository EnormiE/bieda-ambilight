#![windows_subsystem = "windows"]

use hidapi::{HidApi, HidDevice};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::Dialogs::{
    ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, MSG, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW,
};
use xcap::Monitor;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::System::Power::{
    RegisterPowerSettingNotification, POWERBROADCAST_SETTING,
};
use windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE,
    WM_POWERBROADCAST,
};

static POWER_SLEEPING: AtomicBool = AtomicBool::new(false);

const VID: u16 = 0x1A86;
const PID: u16 = 0xFE07;
const USAGE_PAGE: u16 = 0xFF00;
const USAGE: u16 = 0x01;
const TUPLE_COUNT: usize = 34;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    mode: String,
    brightness: u8,
    #[serde(default = "default_50")]
    sensitivity: u8,
    #[serde(default = "default_50")]
    dynamic_speed: u8,
    #[serde(default = "default_sync_fps")]
    sync_fps: String, // "auto", "30", "60"
    #[serde(default)]
    monitor_name: String, // "" = primary monitor
    reverse: bool,
    auto_off: bool,
    #[serde(default = "default_true")]
    rhythm_windows_audio: bool,
    leds_left: usize,
    leds_top: usize,
    leds_right: usize,
    static_color: [u8; 3],
}

fn default_true() -> bool {
    true
}

fn default_50() -> u8 {
    50
}

fn default_sync_fps() -> String {
    "auto".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: "sync_game".to_string(),
            brightness: 230,
            sensitivity: 50,
            dynamic_speed: 50,
            sync_fps: "auto".to_string(),
            monitor_name: String::new(),
            reverse: true,
            auto_off: true,
            rhythm_windows_audio: true,
            leds_left: 12,
            leds_top: 33,
            leds_right: 12,
            static_color: [255, 140, 0],
        }
    }
}

fn ensure_single_instance() -> bool {
    let name: Vec<u16> = "Global\\BiedaAmbilightMutex\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(ptr::null(), 1, name.as_ptr());
        if handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return false;
        }
    }
    true
}

unsafe extern "system" fn shutdown_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                unsafe { PostQuitMessage(0) };
            }
            0
        }
        WM_POWERBROADCAST => {
            match wparam as u32 {
                PBT_APMSUSPEND => POWER_SLEEPING.store(true, Ordering::Relaxed),
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    POWER_SLEEPING.store(false, Ordering::Relaxed);
                }
                PBT_POWERSETTINGCHANGE => {
                    let pbs = lparam as *const POWERBROADCAST_SETTING;
                    if !pbs.is_null() {
                        // 0 = monitor OFF, 1 = ON, 2 = DIMMED
                        let state = unsafe { (*pbs).Data[0] };
                        POWER_SLEEPING.store(state == 0, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
            1
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_shutdown_listener() {
    let class_name: Vec<u16> = "BiedaAmbilightShutdownClass\0".encode_utf16().collect();
    unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(shutdown_wnd_proc);
        wc.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        );
        if !hwnd.is_null() {
            // 0 = DEVICE_NOTIFY_WINDOW_HANDLE
            RegisterPowerSettingNotification(hwnd, &GUID_CONSOLE_DISPLAY_STATE, 0);
        }
    }
}

fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.toml")))
        .unwrap_or_else(|| Path::new("config.toml").to_path_buf())
}

fn load_config() -> Config {
    if let Ok(content) = fs::read_to_string(config_path()) {
        if let Ok(cfg) = toml::from_str(&content) {
            return cfg;
        }
    }
    let cfg = Config::default();
    save_config(&cfg);
    cfg
}

fn save_config(cfg: &Config) {
    if let Ok(text) = toml::to_string_pretty(cfg) {
        let _ = fs::write(config_path(), text);
    }
}

fn is_screen_locked() -> bool {
    unsafe {
        let hdesk = OpenInputDesktop(0, 0, 0x0001);
        if hdesk.is_null() {
            true
        } else {
            CloseDesktop(hdesk);
            false
        }
    }
}

fn pick_color_windows(initial: [u8; 3]) -> Option<[u8; 3]> {
    unsafe {
        let mut cust_colors = [0u32; 16];
        let init_colorref =
            (initial[0] as u32) | ((initial[1] as u32) << 8) | ((initial[2] as u32) << 16);

        let mut cc: CHOOSECOLORW = std::mem::zeroed();
        cc.lStructSize = std::mem::size_of::<CHOOSECOLORW>() as u32;
        cc.rgbResult = init_colorref;
        cc.lpCustColors = cust_colors.as_mut_ptr();
        cc.Flags = CC_FULLOPEN | CC_RGBINIT;

        if ChooseColorW(&mut cc) != 0 {
            let r = (cc.rgbResult & 0xFF) as u8;
            let g = ((cc.rgbResult >> 8) & 0xFF) as u8;
            let b = ((cc.rgbResult >> 16) & 0xFF) as u8;
            Some([r, g, b])
        } else {
            None
        }
    }
}

fn find_monitor(preferred_name: &str) -> Option<Monitor> {
    let monitors = Monitor::all().ok()?;
    if !preferred_name.is_empty() {
        if let Some(m) = monitors
            .iter()
            .find(|m| m.name().ok().as_deref() == Some(preferred_name))
        {
            return Some(m.clone());
        }
    }
    if let Some(primary) = monitors.iter().find(|m| m.is_primary().unwrap_or(false)) {
        return Some(primary.clone());
    }
    monitors.into_iter().next()
}

struct AudioMeter {
    meter: Option<IAudioMeterInformation>,
    last_refresh: Instant,
}

impl AudioMeter {
    fn new() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut s = Self {
            meter: None,
            last_refresh: Instant::now() - Duration::from_secs(10),
        };
        s.refresh();
        s
    }

    fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        self.meter = unsafe {
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .and_then(|en| en.GetDefaultAudioEndpoint(eRender, eConsole))
                .and_then(|dev| dev.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None))
                .ok()
        };
    }

    fn get_volume(&mut self, sensitivity: u8) -> u8 {
        if self.last_refresh.elapsed() >= Duration::from_secs(2) {
            self.refresh();
        }
        if let Some(ref m) = self.meter {
            if let Ok(peak) = unsafe { m.GetPeakValue() } {
                let scale = sensitivity as f32 / 50.0;
                let scaled = (peak * scale).clamp(0.0, 1.0);
                return (scaled * 100.0).round() as u8;
            }
        }
        0
    }
}

struct Robobloq {
    device: HidDevice,
    packet_index: u8,
}

impl Robobloq {
    fn connect() -> Option<Self> {
        let api = HidApi::new().ok()?;
        let dev_info = api.device_list().find(|d| {
            d.vendor_id() == VID
                && d.product_id() == PID
                && d.usage_page() == USAGE_PAGE
                && d.usage() == USAGE
        })?;

        Some(Self {
            device: dev_info.open_device(&api).ok()?,
            packet_index: 0x02,
        })
    }

    fn write_report(&self, data: &[u8]) -> bool {
        let mut report = [0u8; 65];
        report[0] = 0x00;
        report[1..65].copy_from_slice(&data[..64]);
        self.device.write(&report).is_ok()
    }

    fn send_packet(&mut self, command: &[u8]) -> bool {
        let len = (command.len() + 5) as u8;
        let mut packet = [0u8; 64];
        packet[0] = 0x52;
        packet[1] = 0x42;
        packet[2] = len;
        packet[3] = self.packet_index;
        packet[4..4 + command.len()].copy_from_slice(command);

        let csum: u32 = packet[..4 + command.len()].iter().map(|&b| b as u32).sum();
        packet[4 + command.len()] = (csum & 0xFF) as u8;

        let ok = self.write_report(&packet);
        self.packet_index = self.packet_index.wrapping_add(1);
        ok
    }

    fn send_multi_packet(&mut self, command: u8, payload: &[u8]) -> bool {
        let len = (payload.len() + 7) as u16;
        let mut data = Vec::with_capacity(payload.len() + 64);
        data.extend_from_slice(&[
            0x53,
            0x43,
            (len >> 8) as u8,
            (len & 0xFF) as u8,
            self.packet_index,
            command,
        ]);
        data.extend_from_slice(payload);

        let sum: u32 = data.iter().map(|&b| b as u32).sum();
        data.push((sum & 0xFF) as u8);

        let rem = data.len() % 64;
        if rem > 0 {
            data.resize(data.len() + (64 - rem), 0x00);
        }

        for chunk in data.chunks_exact(64) {
            if !self.write_report(chunk) {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }

        self.packet_index = self.packet_index.wrapping_add(1);
        true
    }

    fn set_brightness(&mut self, val: u8) -> bool {
        self.send_packet(&[0x87, val.max(1)])
    }

    fn set_static(&mut self, led_count: u8, rgb: [u8; 3]) -> bool {
        self.send_packet(&[
            0x86,
            0x01,
            rgb[0],
            rgb[1],
            rgb[2],
            led_count,
            led_count + 1,
            0,
            0,
            0,
            0xFE,
        ])
    }

    fn set_effect(&mut self, category: u8, effect: u8) -> bool {
        self.send_packet(&[0x85, category, effect])
    }

    fn set_dynamic_speed(&mut self, speed: u8) -> bool {
        self.send_packet(&[0x8A, speed.clamp(1, 100)])
    }

    fn set_sound_sensitivity(&mut self, sens: u8) -> bool {
        self.send_packet(&[0x8B, sens.clamp(5, 100)])
    }

    fn set_computer_rhythm(&mut self, effect: u8, volume: u8) -> bool {
        self.send_packet(&[0x98, effect, volume.min(100)])
    }

    fn turn_off(&mut self, led_count: u8) -> bool {
        self.send_packet(&[0x97]) && self.set_static(led_count, [0, 0, 0])
    }

    fn send_sync_colors(&mut self, colors: &[[u8; 3]]) -> bool {
        let compressed = merge_robobloq_ranges(colors, TUPLE_COUNT);
        self.send_multi_packet(0x80, &compressed)
    }
}

fn merge_robobloq_ranges(colors: &[[u8; 3]], tuple_count: usize) -> Vec<u8> {
    struct Range {
        start: usize,
        end: usize,
        n: f64,
        sum_r: f64,
        sum_g: f64,
        sum_b: f64,
        term: f64,
    }

    let mut ranges: Vec<Range> = colors
        .iter()
        .enumerate()
        .map(|(i, &[r, g, b])| {
            let (rf, gf, bf) = (r as f64, g as f64, b as f64);
            Range {
                start: i + 1,
                end: i + 1,
                n: 1.0,
                sum_r: rf,
                sum_g: gf,
                sum_b: bf,
                term: rf * rf + gf * gf + bf * bf,
            }
        })
        .collect();

    while ranges.len() > tuple_count {
        let mut best_delta = f64::MAX;
        let mut best_idx = 0;
        let mut best_term = 0.0;

        for i in 0..ranges.len() - 1 {
            let (r1, r2) = (&ranges[i], &ranges[i + 1]);
            let sr = r1.sum_r + r2.sum_r;
            let sg = r1.sum_g + r2.sum_g;
            let sb = r1.sum_b + r2.sum_b;
            let n = r1.n + r2.n;
            let term = (sr * sr + sg * sg + sb * sb) / n;
            let delta = r1.term + r2.term - term;

            if delta < best_delta {
                best_delta = delta;
                best_idx = i;
                best_term = term;
            }
        }

        let right = ranges.remove(best_idx + 1);
        let left = &mut ranges[best_idx];
        left.end = right.end;
        left.n += right.n;
        left.sum_r += right.sum_r;
        left.sum_g += right.sum_g;
        left.sum_b += right.sum_b;
        left.term = best_term;
    }

    let mut out = Vec::with_capacity(ranges.len() * 5);
    for r in ranges {
        out.push(r.start as u8);
        out.push((r.sum_r / r.n).round() as u8);
        out.push((r.sum_g / r.n).round() as u8);
        out.push((r.sum_b / r.n).round() as u8);
        out.push(r.end as u8);
    }
    out
}

fn avg_rect(img: &xcap::image::RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32) -> [u8; 3] {
    let (mut r, mut g, mut b, mut count) = (0u64, 0u64, 0u64, 0u64);
    for y in (y0..y1).step_by(4) {
        for x in (x0..x1).step_by(4) {
            let p = img.get_pixel(x, y);
            r += p[0] as u64;
            g += p[1] as u64;
            b += p[2] as u64;
            count += 1;
        }
    }
    if count == 0 {
        return [0, 0, 0];
    }
    [(r / count) as u8, (g / count) as u8, (b / count) as u8]
}

fn rhythm_effect_index(mode: &str) -> Option<u8> {
    match mode {
        "rhy_circles" => Some(0),
        "rhy_twirl" => Some(1),
        "rhy_sparkle" => Some(2),
        "rhy_bubbles" => Some(3),
        "rhy_spotlight" => Some(4),
        "rhy_rainbow" => Some(5),
        "rhy_blast" => Some(6),
        _ => None,
    }
}

fn dynamic_effect_index(mode: &str) -> Option<u8> {
    match mode {
        "dyn_rainbow" => Some(0),
        "dyn_breathing" => Some(1),
        "dyn_twist" => Some(2),
        "dyn_beat" => Some(3),
        "dyn_twirl" => Some(4),
        "dyn_lemon" => Some(5),
        "dyn_electric" => Some(6),
        _ => None,
    }
}

fn target_sync_interval(cfg: &Config) -> Duration {
    match cfg.sync_fps.as_str() {
        "30" => Duration::from_millis(33),
        "60" => Duration::from_millis(16),
        _ => {
            if cfg.mode == "sync_movie" {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(16)
            }
        }
    }
}

#[derive(Clone)]
enum WorkerCmd {
    SetMode(String),
    SetBrightness(u8),
    SetSensitivity(u8),
    SetDynamicSpeed(u8),
    SetSyncFps(String),
    SetMonitor(String),
    SetStaticColor([u8; 3]),
    ToggleReverse,
    ToggleAutoOff,
    ToggleRhythmSource,
    Quit,
}

fn apply_mode(strip: &mut Robobloq, cfg: &Config) -> bool {
    let count = (cfg.leds_left + cfg.leds_top + cfg.leds_right) as u8;
    if !strip.set_brightness(cfg.brightness) {
        return false;
    }

    if let Some(rhy_idx) = rhythm_effect_index(&cfg.mode) {
        if !cfg.rhythm_windows_audio {
            return strip.set_sound_sensitivity(cfg.sensitivity)
                && strip.set_effect(0x03, rhy_idx);
        }
        return true;
    }

    if let Some(dyn_idx) = dynamic_effect_index(&cfg.mode) {
        return strip.set_dynamic_speed(cfg.dynamic_speed) && strip.set_effect(0x02, dyn_idx);
    }

    match cfg.mode.as_str() {
        "static" => strip.set_static(count, cfg.static_color),
        "off" => strip.turn_off(count),
        _ => true,
    }
}

fn worker_loop(rx: Receiver<WorkerCmd>, mut cfg: Config) {
    let mut strip = Robobloq::connect();
    let mut last_reconnect = Instant::now();
    let mut monitor = find_monitor(&cfg.monitor_name);
    let mut audio = AudioMeter::new();
    let total_leds = cfg.leds_left + cfg.leds_top + cfg.leds_right;
    let mut prev_colors = vec![[0u8; 3]; total_leds];
    let mut sleeping = false;

    if let Some(ref mut s) = strip {
        if !apply_mode(s, &cfg) {
            strip = None;
        }
    }

    let mut next_tick = Instant::now();

    loop {
        if strip.is_none() && last_reconnect.elapsed() >= Duration::from_secs(2) {
            last_reconnect = Instant::now();
            if let Some(mut s) = Robobloq::connect() {
                if !sleeping {
                    let _ = apply_mode(&mut s, &cfg);
                }
                strip = Some(s);
            }
        }

        let is_sync = cfg.mode == "sync_game" || cfg.mode == "sync_movie";
        let win_rhythm_idx = if cfg.rhythm_windows_audio {
            rhythm_effect_index(&cfg.mode)
        } else {
            None
        };

        let wait_time = if sleeping || strip.is_none() {
            Duration::from_millis(500)
        } else if is_sync || win_rhythm_idx.is_some() {
            next_tick.saturating_duration_since(Instant::now())
        } else {
            Duration::from_millis(500)
        };

        if let Ok(cmd) = rx.recv_timeout(wait_time) {
            let mut ok = true;
            match cmd {
                WorkerCmd::SetMode(m) => {
                    cfg.mode = m;
                    save_config(&cfg);
                    if !sleeping {
                        if let Some(ref mut s) = strip {
                            ok = apply_mode(s, &cfg);
                        }
                    }
                    next_tick = Instant::now();
                }
                WorkerCmd::SetBrightness(b) => {
                    cfg.brightness = b;
                    save_config(&cfg);
                    if !sleeping {
                        if let Some(ref mut s) = strip {
                            ok = s.set_brightness(b);
                        }
                    }
                }
                WorkerCmd::SetSensitivity(sens) => {
                    cfg.sensitivity = sens;
                    save_config(&cfg);
                    if !sleeping
                        && !cfg.rhythm_windows_audio
                        && rhythm_effect_index(&cfg.mode).is_some()
                    {
                        if let Some(ref mut s) = strip {
                            ok = s.set_sound_sensitivity(sens);
                        }
                    }
                }
                WorkerCmd::SetDynamicSpeed(speed) => {
                    cfg.dynamic_speed = speed;
                    save_config(&cfg);
                    if !sleeping && dynamic_effect_index(&cfg.mode).is_some() {
                        if let Some(ref mut s) = strip {
                            ok = s.set_dynamic_speed(speed);
                        }
                    }
                }
                WorkerCmd::SetSyncFps(fps) => {
                    cfg.sync_fps = fps;
                    save_config(&cfg);
                    next_tick = Instant::now();
                }
                WorkerCmd::SetMonitor(name) => {
                    cfg.monitor_name = name;
                    save_config(&cfg);
                    monitor = find_monitor(&cfg.monitor_name);
                }
                WorkerCmd::SetStaticColor(rgb) => {
                    cfg.static_color = rgb;
                    cfg.mode = "static".to_string();
                    save_config(&cfg);
                    if !sleeping {
                        if let Some(ref mut s) = strip {
                            ok = apply_mode(s, &cfg);
                        }
                    }
                }
                WorkerCmd::ToggleReverse => {
                    cfg.reverse = !cfg.reverse;
                    save_config(&cfg);
                }
                WorkerCmd::ToggleAutoOff => {
                    cfg.auto_off = !cfg.auto_off;
                    save_config(&cfg);
                }
                WorkerCmd::ToggleRhythmSource => {
                    cfg.rhythm_windows_audio = !cfg.rhythm_windows_audio;
                    save_config(&cfg);
                    if !sleeping {
                        if let Some(ref mut s) = strip {
                            ok = apply_mode(s, &cfg);
                        }
                    }
                }
                WorkerCmd::Quit => {
                    if let Some(ref mut s) = strip {
                        let _ = s.turn_off(total_leds as u8);
                    }
                    break;
                }
            }
            if !ok {
                strip = None;
            }
            continue;
        }

        if cfg.auto_off {
            let locked = is_screen_locked() || POWER_SLEEPING.load(Ordering::Relaxed);
            if locked && !sleeping {
                sleeping = true;
                if let Some(ref mut s) = strip {
                    if !s.turn_off(total_leds as u8) {
                        strip = None;
                    }
                }
                continue;
            } else if !locked && sleeping {
                sleeping = false;
                if let Some(ref mut s) = strip {
                    if !apply_mode(s, &cfg) {
                        strip = None;
                    }
                }
                next_tick = Instant::now();
            }
        } else if sleeping {
            sleeping = false;
            if let Some(ref mut s) = strip {
                if !apply_mode(s, &cfg) {
                    strip = None;
                }
            }
            next_tick = Instant::now();
        }

        if sleeping || strip.is_none() {
            continue;
        }

        if let Some(effect_idx) = win_rhythm_idx {
            let frame_start = Instant::now();
            let vol = audio.get_volume(cfg.sensitivity);
            if let Some(ref mut s) = strip {
                if !s.set_computer_rhythm(effect_idx, vol) {
                    strip = None;
                }
            }
            next_tick = frame_start + Duration::from_millis(25);
        } else if is_sync {
            let frame_start = Instant::now();
            let capture_res = monitor.as_ref().and_then(|m| m.capture_image().ok());

            if let Some(img) = capture_res {
                let movie = cfg.mode == "sync_movie";
                let alpha = if movie { 0.25 } else { 0.85 };
                let top_offset_pct = if movie { 0.12 } else { 0.0 };

                let (w, h) = (img.width(), img.height());
                let depth_x = (w as f32 * 0.08) as u32;
                let depth_y = (h as f32 * 0.08) as u32;
                let top_y0 = (h as f32 * top_offset_pct) as u32;

                let mut colors = Vec::with_capacity(total_leds);

                if cfg.leds_left > 0 {
                    for i in 0..cfg.leds_left {
                        let y1 = h - (i as u32 * h / cfg.leds_left as u32);
                        let y0 = h - ((i + 1) as u32 * h / cfg.leds_left as u32);
                        colors.push(avg_rect(&img, 0, y0, depth_x, y1));
                    }
                }
                if cfg.leds_top > 0 {
                    for i in 0..cfg.leds_top {
                        let x0 = i as u32 * w / cfg.leds_top as u32;
                        let x1 = (i + 1) as u32 * w / cfg.leds_top as u32;
                        colors.push(avg_rect(&img, x0, top_y0, x1, (top_y0 + depth_y).min(h)));
                    }
                }
                if cfg.leds_right > 0 {
                    for i in 0..cfg.leds_right {
                        let y0 = i as u32 * h / cfg.leds_right as u32;
                        let y1 = (i + 1) as u32 * h / cfg.leds_right as u32;
                        colors.push(avg_rect(&img, w - depth_x, y0, w, y1));
                    }
                }

                if cfg.reverse {
                    colors.reverse();
                }

                for (curr, prev) in colors.iter_mut().zip(prev_colors.iter_mut()) {
                    for c in 0..3 {
                        let blended =
                            (curr[c] as f32 * alpha) + (prev[c] as f32 * (1.0 - alpha));
                        curr[c] = blended.round() as u8;
                        prev[c] = curr[c];
                    }
                }

                if !colors.is_empty() {
                    if let Some(ref mut s) = strip {
                        if !s.send_sync_colors(&colors) {
                            strip = None;
                        }
                    }
                }
            } else {
                // Refresh the monitor handle after a resolution change or system wake-up.
                monitor = find_monitor(&cfg.monitor_name);
            }

            next_tick = frame_start + target_sync_interval(&cfg);
        }
    }
}

fn create_tray_icon() -> Icon {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            rgba.push((x * 16) as u8);
            rgba.push((y * 16) as u8);
            rgba.push(200);
            rgba.push(255);
        }
    }
    Icon::from_rgba(rgba, 16, 16).unwrap()
}

fn main() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    if !ensure_single_instance() {
        return;
    }

    create_shutdown_listener();

    let cfg = load_config();
    let mut current_color = cfg.static_color;
    let (tx, rx) = mpsc::channel::<WorkerCmd>();

    let worker_cfg = cfg.clone();
    let worker = thread::spawn(move || worker_loop(rx, worker_cfg));

    let mut actions: HashMap<MenuId, WorkerCmd> = HashMap::new();
    let mut mode_items: Vec<(String, CheckMenuItem)> = Vec::new();
    let mut bright_items: Vec<CheckMenuItem> = Vec::new();
    let mut speed_items: Vec<CheckMenuItem> = Vec::new();
    let mut sens_items: Vec<CheckMenuItem> = Vec::new();
    let mut fps_items: Vec<CheckMenuItem> = Vec::new();
    let mut mon_items: Vec<CheckMenuItem> = Vec::new();

    let mut add_mode = |label: &str, key: &str| -> CheckMenuItem {
        let item = CheckMenuItem::new(label, true, cfg.mode == key, None);
        actions.insert(item.id().clone(), WorkerCmd::SetMode(key.into()));
        mode_items.push((key.into(), item.clone()));
        item
    };

    let menu = Menu::new();

    // Screen Sync
    let i_game = add_mode("Sync: Gaming", "sync_game");
    let i_movie = add_mode("Sync: Movies", "sync_movie");

    // Static Color
    let i_static = add_mode("Static Color", "static");
    let i_pick_color = MenuItem::new("Choose Color...", true, None);
    let id_pick_color = i_pick_color.id().clone();

    // Submenu: Dynamic (7 effects)
    let sub_dyn = Submenu::new("Dynamic", true);
    for (label, key) in [
        ("Rainbow Wave", "dyn_rainbow"),
        ("Breathing", "dyn_breathing"),
        ("Twist", "dyn_twist"),
        ("Beat", "dyn_beat"),
        ("Twirl", "dyn_twirl"),
        ("Lemon", "dyn_lemon"),
        ("Electric", "dyn_electric"),
    ] {
        let item = add_mode(label, key);
        let _ = sub_dyn.append(&item);
    }

    // Submenu: Rhythm (7 effects)
    let sub_rhy = Submenu::new("Rhythm (Music)", true);
    for (label, key) in [
        ("Circles", "rhy_circles"),
        ("Twirl", "rhy_twirl"),
        ("Sparkle", "rhy_sparkle"),
        ("Bubbles", "rhy_bubbles"),
        ("Spotlight", "rhy_spotlight"),
        ("Rainbow", "rhy_rainbow"),
        ("Blast", "rhy_blast"),
    ] {
        let item = add_mode(label, key);
        let _ = sub_rhy.append(&item);
    }

    // Turn Off (before releasing add_mode)
    let i_off = add_mode("Turn LEDs Off", "off");

    // Submenu: Brightness (10%–100%)
    let sub_bright = Submenu::new("Brightness", true);
    let closest_bright_pct =
        (((cfg.brightness as f32 / 255.0) * 10.0).round() as u8 * 10).clamp(10, 100);
    for pct in (10..=100).step_by(10) {
        let val = ((pct as f32 / 100.0) * 255.0).round() as u8;
        let item = CheckMenuItem::new(format!("{}%", pct), true, pct == closest_bright_pct, None);
        actions.insert(item.id().clone(), WorkerCmd::SetBrightness(val));
        bright_items.push(item.clone());
        let _ = sub_bright.append(&item);
    }

    // Submenu: Animation Speed (10%–100%)
    let sub_speed = Submenu::new("Animation Speed", true);
    let closest_speed_pct = ((cfg.dynamic_speed as f32 / 10.0).round() as u8 * 10).clamp(10, 100);
    for pct in (10..=100).step_by(10) {
        let item = CheckMenuItem::new(format!("{}%", pct), true, pct == closest_speed_pct, None);
        actions.insert(item.id().clone(), WorkerCmd::SetDynamicSpeed(pct));
        speed_items.push(item.clone());
        let _ = sub_speed.append(&item);
    }

    // Submenu: Audio Sensitivity (10%–100%)
    let sub_sens = Submenu::new("Audio Sensitivity", true);
    let closest_sens_pct = ((cfg.sensitivity as f32 / 10.0).round() as u8 * 10).clamp(10, 100);
    for pct in (10..=100).step_by(10) {
        let item = CheckMenuItem::new(format!("{}%", pct), true, pct == closest_sens_pct, None);
        actions.insert(item.id().clone(), WorkerCmd::SetSensitivity(pct));
        sens_items.push(item.clone());
        let _ = sub_sens.append(&item);
    }

    // Submenu: Sync Refresh Rate
    let sub_fps = Submenu::new("Sync Refresh Rate", true);
    for (label, key) in [
        ("Auto (Movies 30 / Games 60)", "auto"),
        ("30 FPS (Power Saving)", "30"),
        ("60 FPS (High Performance)", "60"),
    ] {
        let item = CheckMenuItem::new(label, true, cfg.sync_fps == key, None);
        actions.insert(item.id().clone(), WorkerCmd::SetSyncFps(key.into()));
        fps_items.push(item.clone());
        let _ = sub_fps.append(&item);
    }

    // Submenu: Monitor
    let sub_mon = Submenu::new("Monitor", true);
    let mon_auto = CheckMenuItem::new("Primary (Auto)", true, cfg.monitor_name.is_empty(), None);
    actions.insert(mon_auto.id().clone(), WorkerCmd::SetMonitor(String::new()));
    mon_items.push(mon_auto.clone());
    let _ = sub_mon.append(&mon_auto);

    if let Ok(monitors) = Monitor::all() {
        for (idx, m) in monitors.iter().enumerate() {
            let name = m.name().unwrap_or_else(|_| format!("Monitor {}", idx + 1));
            let label = if m.is_primary().unwrap_or(false) {
                format!("{} (Primary)", name)
            } else {
                name.clone()
            };
            let item = CheckMenuItem::new(&label, true, cfg.monitor_name == name, None);
            actions.insert(item.id().clone(), WorkerCmd::SetMonitor(name));
            mon_items.push(item.clone());
            let _ = sub_mon.append(&item);
        }
    }

    // Settings checkboxes
    let chk_rhythm_src = CheckMenuItem::new(
        "Music: Windows Audio",
        true,
        cfg.rhythm_windows_audio,
        None,
    );
    let chk_reverse = CheckMenuItem::new("Reverse Direction (Left/Right)", true, cfg.reverse, None);
    let chk_auto_off = CheckMenuItem::new("Turn Off When Screen Is Off", true, cfg.auto_off, None);
    actions.insert(chk_rhythm_src.id().clone(), WorkerCmd::ToggleRhythmSource);
    actions.insert(chk_reverse.id().clone(), WorkerCmd::ToggleReverse);
    actions.insert(chk_auto_off.id().clone(), WorkerCmd::ToggleAutoOff);

    // Exit
    let i_quit = MenuItem::new("Exit Application", true, None);
    let id_quit = i_quit.id().clone();

    let _ = menu.append_items(&[
        &i_game,
        &i_movie,
        &PredefinedMenuItem::separator(),
        &i_static,
        &i_pick_color,
        &sub_dyn,
        &sub_rhy,
        &PredefinedMenuItem::separator(),
        &sub_bright,
        &sub_speed,
        &sub_sens,
        &sub_fps,
        &sub_mon,
        &chk_rhythm_src,
        &chk_reverse,
        &chk_auto_off,
        &PredefinedMenuItem::separator(),
        &i_off,
        &i_quit,
    ]);

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("bieda-ambilight")
        .with_icon(create_tray_icon())
        .build()
        .unwrap();

    let menu_rx = MenuEvent::receiver();

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);

            let mut should_quit = false;
            while let Ok(event) = menu_rx.try_recv() {
                if event.id == id_pick_color {
                    if let Some(rgb) = pick_color_windows(current_color) {
                        current_color = rgb;
                        for (key, item) in &mode_items {
                            item.set_checked(key == "static");
                        }
                        let _ = tx.send(WorkerCmd::SetStaticColor(rgb));
                    }
                } else if event.id == id_quit {
                    should_quit = true;
                    break;
                } else if let Some(cmd) = actions.get(&event.id) {
                    match cmd {
                        WorkerCmd::SetMode(selected_key) => {
                            for (key, item) in &mode_items {
                                item.set_checked(key == selected_key);
                            }
                        }
                        WorkerCmd::SetBrightness(_) => {
                            for item in &bright_items {
                                item.set_checked(item.id() == &event.id);
                            }
                        }
                        WorkerCmd::SetDynamicSpeed(_) => {
                            for item in &speed_items {
                                item.set_checked(item.id() == &event.id);
                            }
                        }
                        WorkerCmd::SetSensitivity(_) => {
                            for item in &sens_items {
                                item.set_checked(item.id() == &event.id);
                            }
                        }
                        WorkerCmd::SetSyncFps(_) => {
                            for item in &fps_items {
                                item.set_checked(item.id() == &event.id);
                            }
                        }
                        WorkerCmd::SetMonitor(_) => {
                            for item in &mon_items {
                                item.set_checked(item.id() == &event.id);
                            }
                        }
                        _ => {}
                    }
                    let _ = tx.send(cmd.clone());
                }
            }
            if should_quit {
                break;
            }
        }
    }

    let _ = tx.send(WorkerCmd::Quit);
    let _ = worker.join();
}