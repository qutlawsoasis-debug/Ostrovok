//! «Пульт»: яркость монитора (DDC/CI), батарея ноутбука, Bluetooth-устройства с зарядом.
//! Опрос идёт в фоновом потоке, страница только читает готовое состояние.

use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct BtDevice {
    pub name: String,
    pub battery: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub pct: u8,
    pub charging: bool,
    /// секунд до разряда, если Windows знает
    pub secs_left: Option<u32>,
}

#[derive(Default)]
pub struct State {
    /// яркость 0..=100; None — монитор не поддерживает DDC/CI, плитку прячем
    pub brightness: Option<u8>,
    pub battery: Option<Battery>,
    pub devices: Vec<BtDevice>,
    /// только что подключённые наушники — остров покажет короткую плашку
    pub events: Vec<BtDevice>,
    /// желаемая яркость, которую поток применит
    pub want_brightness: Option<u8>,
    /// первый опрос уже был (чтобы не объявлять наушники, подключённые до запуска)
    pub primed: bool,
    /// для снимков: образцы не перезаписываются опросом
    pub frozen: bool,
    /// страница «Пульт» сейчас на экране — опрос чаще
    pub visible: bool,
}

// ───────────── чистая логика ─────────────

/// Служебные узлы Bluetooth, которые не являются устройствами пользователя.
pub fn is_user_device(name: &str) -> bool {
    let low = name.to_lowercase();
    // только явные служебные узлы: «JBL Radio» или «Xbox Adapter» — устройства пользователя
    let junk = ["microsoft bluetooth", "bluetooth le enumerator", "bluetooth device (rfcomm", "bluetooth device (personal area", "generic attribute profile", "generic access profile", "device identification service", "avrcp transport", "intel(r) wireless bluetooth", "realtek bluetooth", "qualcomm", "mediatek bluetooth", "broadcom bluetooth"];
    !name.trim().is_empty() && !junk.iter().any(|j| low.starts_with(j))
}

/// Похоже ли имя на наушники/гарнитуру.
pub fn is_headphones(name: &str) -> bool {
    let low = name.to_lowercase();
    ["airpods", "buds", "headphone", "headset", "наушник", "гарнитур", "earbud", "wh-", "wf-", "jbl", "beats", "freebuds", "airdots", "hands-free", "a2dp"].iter().any(|k| low.contains(k))
}

/// Разбор строк «имя\tзаряд» из PowerShell: служебное отбрасываем, одинаковые имена схлопываем,
/// заряд берём, где он есть (у одного устройства бывает несколько узлов).
pub fn parse_devices(text: &str) -> Vec<BtDevice> {
    let mut out: Vec<BtDevice> = Vec::new();
    for line in text.lines() {
        let mut it = line.splitn(2, '\t');
        let name = it.next().unwrap_or("").trim();
        let battery = it.next().and_then(|b| b.trim().parse::<u8>().ok()).filter(|b| *b <= 100);
        if !is_user_device(name) {
            continue;
        }
        // «Avantree (Hands-Free)» и «Avantree» — одно устройство
        let base = name.split(" (").next().unwrap_or(name).trim().to_string();
        match out.iter_mut().find(|d| d.name == base) {
            Some(d) => d.battery = d.battery.or(battery),
            None => out.push(BtDevice { name: base, battery }),
        }
    }
    out
}

/// Наушники, появившиеся в новом списке и отсутствовавшие в старом.
pub fn new_headphones(old: &[BtDevice], new: &[BtDevice]) -> Vec<BtDevice> {
    new.iter().filter(|d| is_headphones(&d.name) && !old.iter().any(|o| o.name == d.name)).cloned().collect()
}

/// Текст плашки: «Наушники подключены · 80%».
pub fn headphones_text(d: &BtDevice) -> String {
    match d.battery {
        Some(b) => format!("Наушники подключены · {b}%"),
        None => "Наушники подключены".to_string(),
    }
}

/// «2 ч 15 мин», «45 мин», «меньше минуты».
pub fn fmt_duration(secs: u32) -> String {
    let m = secs / 60;
    match (m / 60, m % 60) {
        (0, 0) => "меньше минуты".into(),
        (0, m) => format!("{m} мин"),
        (h, 0) => format!("{h} ч"),
        (h, m) => format!("{h} ч {m} мин"),
    }
}

/// Строка под процентом батареи.
pub fn battery_status(b: &Battery) -> String {
    if b.charging {
        if b.pct >= 100 { "заряжена, от сети".into() } else { "заряжается".into() }
    } else {
        match b.secs_left {
            Some(s) => format!("осталось {}", fmt_duration(s)),
            None => "от батареи".into(),
        }
    }
}

/// Значок Segoe для уровня заряда (с молнией при зарядке).
pub fn battery_glyph(pct: u8, charging: bool) -> &'static str {
    const PLAIN: [&str; 11] = ["\u{E850}", "\u{E851}", "\u{E852}", "\u{E853}", "\u{E854}", "\u{E855}", "\u{E856}", "\u{E857}", "\u{E858}", "\u{E859}", "\u{E83F}"];
    const CHARGE: [&str; 11] = ["\u{E85A}", "\u{E85B}", "\u{E85C}", "\u{E85D}", "\u{E85E}", "\u{E85F}", "\u{E860}", "\u{E861}", "\u{E862}", "\u{E83E}", "\u{EA93}"];
    let i = (pct.min(100) as usize + 5) / 10;
    if charging { CHARGE[i] } else { PLAIN[i] }
}

/// Значок устройства по имени.
pub fn device_glyph(name: &str) -> &'static str {
    let low = name.to_lowercase();
    if is_headphones(name) {
        "\u{E7F6}"
    } else if low.contains("mouse") || low.contains("мыш") || low.contains("mx ") {
        "\u{E962}"
    } else if low.contains("keyboard") || low.contains("клавиат") || low.contains("keys") {
        "\u{E765}"
    } else if low.contains("controller") || low.contains("gamepad") || low.contains("xbox") || low.contains("dualsense") {
        "\u{E7FC}"
    } else if low.contains("speaker") || low.contains("колонк") {
        "\u{E7F5}"
    } else {
        "\u{E702}"
    }
}

/// Показывать ли уведомление мессенджера плашкой: в режиме «Не беспокоить» — нет
/// (в «Уведомления» оно всё равно попадает).
pub fn should_toast(dnd: bool) -> bool {
    !dnd
}

// ───────────── опрос Windows ─────────────

/// Как часто опрашивать Bluetooth (секунды): страница открыта — часто, иначе редко,
/// только чтобы заметить подключённые наушники.
pub fn bt_poll_secs(visible: bool) -> u64 {
    if visible { 10 } else { 90 }
}

pub fn start(state: Arc<Mutex<State>>, ctx: eframe::egui::Context) {
    // яркость и батарея: дешёвый поток, отвечает на ползунок без задержки
    {
        let state = state.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut tick = 0u32;
            loop {
                let (want, visible, frozen, known) = match state.lock() {
                    Ok(mut s) => (s.want_brightness.take(), s.visible, s.frozen, s.brightness.is_some()),
                    Err(_) => return,
                };
                if frozen {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                if let Some(v) = want {
                    if set_brightness(v) {
                        if let Ok(mut s) = state.lock() {
                            s.brightness = Some(v);
                        }
                    }
                }
                // раз в 10 с при открытой странице; DDC/CI медленный, поэтому в фоне не трогаем
                if tick % 20 == 0 && (visible || tick == 0) {
                    let bright = if known || tick % 120 == 0 { brightness() } else { None };
                    let bat = battery();
                    if let Ok(mut s) = state.lock() {
                        if s.want_brightness.is_none() {
                            s.brightness = bright;
                        }
                        s.battery = bat;
                    }
                    ctx.request_repaint();
                }
                tick = tick.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(if visible { 250 } else { 500 }));
            }
        });
    }
    // Bluetooth: внешний процесс, поэтому отдельный поток и редкий опрос
    std::thread::spawn(move || {
        let mut last: Option<std::time::Instant> = None;
        loop {
            let (visible, frozen) = match state.lock() {
                Ok(s) => (s.visible, s.frozen),
                Err(_) => return,
            };
            let due = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(bt_poll_secs(visible)));
            // в полноэкранной игре PowerShell не запускаем совсем: опросим после игры
            if !frozen && due && crate::quiet::allowed(crate::quiet::Job::Bluetooth, crate::quiet::in_game()) {
                last = Some(std::time::Instant::now());
                if let Some(devs) = bt_devices() {
                    if let Ok(mut s) = state.lock() {
                        if s.primed {
                            let fresh = new_headphones(&s.devices, &devs);
                            s.events.extend(fresh);
                        }
                        s.devices = devs;
                        s.primed = true;
                    }
                    ctx.request_repaint();
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn battery() -> Option<Battery> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut p = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut p).ok()? };
    // 128 — батареи нет, 255 — неизвестно
    if p.BatteryFlag & 128 != 0 || p.BatteryFlag == 255 || p.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery { pct: p.BatteryLifePercent, charging: p.ACLineStatus == 1, secs_left: (p.BatteryLifeTime != u32::MAX).then_some(p.BatteryLifeTime) })
}

/// Физические мониторы, освобождаются в Drop (и при панике тоже).
struct Monitors(Vec<windows::Win32::Devices::Display::PHYSICAL_MONITOR>);

impl Drop for Monitors {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Devices::Display::DestroyPhysicalMonitors(&self.0);
        }
    }
}

fn with_primary_monitor<T>(f: impl FnOnce(windows::Win32::Foundation::HANDLE) -> Option<T>) -> Option<T> {
    use windows::Win32::Devices::Display::*;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
    let mons = unsafe {
        let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut n = 0u32;
        GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut n).ok()?;
        if n == 0 || n > 8 {
            return None;
        }
        let mut arr = vec![PHYSICAL_MONITOR::default(); n as usize];
        GetPhysicalMonitorsFromHMONITOR(hmon, &mut arr).ok()?;
        Monitors(arr)
    };
    f(mons.0[0].hPhysicalMonitor)
}

fn brightness() -> Option<u8> {
    use windows::Win32::Devices::Display::GetMonitorBrightness;
    with_primary_monitor(|h| unsafe {
        let (mut lo, mut cur, mut hi) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(h, &mut lo, &mut cur, &mut hi) == 0 || hi <= lo {
            return None;
        }
        Some(((cur.clamp(lo, hi) - lo) * 100 / (hi - lo)) as u8)
    })
}

/// true — монитор принял новую яркость.
fn set_brightness(v: u8) -> bool {
    use windows::Win32::Devices::Display::{GetMonitorBrightness, SetMonitorBrightness};
    with_primary_monitor(|h| unsafe {
        let (mut lo, mut cur, mut hi) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(h, &mut lo, &mut cur, &mut hi) == 0 || hi <= lo {
            return None;
        }
        (SetMonitorBrightness(h, lo + (hi - lo) * v.min(100) as u32 / 100) != 0).then_some(())
    })
    .is_some()
}

/// Подключённые Bluetooth-устройства и заряд (DEVPKEY_Bluetooth_Battery), через PowerShell без окна.
/// Скрипт постоянный, без пользовательского ввода; зависший процесс снимается через 8 с.
fn bt_devices() -> Option<Vec<BtDevice>> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL: u32 = 0x0000_4000;
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
        Get-PnpDevice -Class Bluetooth,AudioEndpoint -PresentOnly -ErrorAction SilentlyContinue | Where-Object { $_.Status -eq 'OK' -and $_.InstanceId -like 'BTH*' } | ForEach-Object { \
        $p = Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName '{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2','{83DA6326-97A6-4088-9453-A1923F573B29} 15' -ErrorAction SilentlyContinue; \
        $b = ($p | Where-Object KeyName -like '{104EA319*').Data; $c = ($p | Where-Object KeyName -like '{83DA6326*').Data; \
        if ($c -ne $false) { \"$($_.FriendlyName)`t$b\" } }";
    // полный путь: не подхватить чужой powershell.exe из PATH
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut child = Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(NO_WINDOW | BELOW_NORMAL)
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if started.elapsed() < Duration::from_secs(8) => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    status.success().then(|| parse_devices(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: &str, b: Option<u8>) -> BtDevice {
        BtDevice { name: name.into(), battery: b }
    }

    #[test]
    fn parse_skips_service_nodes_and_merges() {
        let txt = "Microsoft Bluetooth LE Enumerator\t\nSony WH-1000XM4\t80\nSony WH-1000XM4 (Hands-Free)\t\nMX Master 3\t\nIntel(R) Wireless Bluetooth(R)\t\nBad\t300\n";
        let v = parse_devices(txt);
        assert_eq!(v, vec![d("Sony WH-1000XM4", Some(80)), d("MX Master 3", None), d("Bad", None)]);
    }

    #[test]
    fn parse_takes_battery_from_any_node() {
        let v = parse_devices("Buds2 (Hands-Free)\t\nBuds2\t55\n");
        assert_eq!(v, vec![d("Buds2", Some(55))]);
    }

    #[test]
    fn headphones_detected_only_when_new() {
        let old = vec![d("MX Master 3", None)];
        let new = vec![d("MX Master 3", Some(40)), d("AirPods Pro", Some(80))];
        assert_eq!(new_headphones(&old, &new), vec![d("AirPods Pro", Some(80))]);
        assert!(new_headphones(&new, &new).is_empty());
    }

    #[test]
    fn headphones_text_formats() {
        assert_eq!(headphones_text(&d("x", Some(80))), "Наушники подключены · 80%");
        assert_eq!(headphones_text(&d("x", None)), "Наушники подключены");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(30), "меньше минуты");
        assert_eq!(fmt_duration(45 * 60), "45 мин");
        assert_eq!(fmt_duration(7200), "2 ч");
        assert_eq!(fmt_duration(8100), "2 ч 15 мин");
    }

    #[test]
    fn battery_lines() {
        assert_eq!(battery_status(&Battery { pct: 50, charging: true, secs_left: None }), "заряжается");
        assert_eq!(battery_status(&Battery { pct: 100, charging: true, secs_left: None }), "заряжена, от сети");
        assert_eq!(battery_status(&Battery { pct: 50, charging: false, secs_left: Some(3900) }), "осталось 1 ч 5 мин");
        assert_eq!(battery_status(&Battery { pct: 50, charging: false, secs_left: None }), "от батареи");
    }

    #[test]
    fn glyphs_cover_range() {
        assert_eq!(battery_glyph(0, false), "\u{E850}");
        assert_eq!(battery_glyph(100, false), "\u{E83F}");
        assert_eq!(battery_glyph(255, true), "\u{EA93}");
        assert_eq!(device_glyph("Logitech Mouse"), "\u{E962}");
        assert_eq!(device_glyph("Galaxy Buds"), "\u{E7F6}");
        assert_eq!(device_glyph("Что-то"), "\u{E702}");
    }

    #[test]
    fn user_devices_not_overfiltered() {
        assert!(is_user_device("JBL Radio"));
        assert!(is_user_device("Xbox Wireless Adapter"));
        assert!(!is_user_device("Microsoft Bluetooth LE Enumerator"));
        assert!(!is_user_device("Intel(R) Wireless Bluetooth(R)"));
    }

    #[test]
    fn bt_poll_rare_in_background() {
        assert_eq!(bt_poll_secs(true), 10);
        assert!(bt_poll_secs(false) >= 60);
    }

    #[test]
    fn dnd_mutes_toasts() {
        assert!(should_toast(false));
        assert!(!should_toast(true));
    }
}
