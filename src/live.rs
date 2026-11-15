//! Живые события: уведомления мессенджеров и звонок в Discord.
//!
//! Подход перенесён из Atoll (Services/NotificationMirror.cs и AudioService.cs, лицензия MIT):
//! - уведомления читаются из системного списка уведомлений Windows — только если пользователь сам
//!   включил это в настройках и разрешил в окне Windows; берутся только мессенджеры;
//! - звонок определяется по тому, что Discord прямо сейчас использует микрофон.
//! Ничего из этого никуда не отправляется: данные показываются на острове и забываются.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use windows::core::Interface;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds};

const MESSENGERS: [&str; 12] = ["telegram", "discord", "viber", "whatsapp", "signal", "slack", "teams", "skype", "vk", "messenger", "element", "zoom"];

/// Доступ к уведомлениям: 0 — не запрашивали, 1 — разрешён, 2 — запрещён, 3 — недоступен.
pub struct Live {
    pub discord: Arc<AtomicBool>,
    pub mirror: Arc<AtomicBool>,
    pub access: Arc<AtomicU8>,
}

pub struct Notice {
    pub app: String,
    pub title: String,
    pub body: String,
}

pub fn spawn(mirror_on: bool, report: impl Fn(Notice) + Send + 'static) -> Live {
    let live = Live {
        discord: Arc::new(AtomicBool::new(false)),
        mirror: Arc::new(AtomicBool::new(mirror_on)),
        access: Arc::new(AtomicU8::new(0)),
    };
    let (discord, mirror, access) = (live.discord.clone(), live.mirror.clone(), live.access.clone());
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut listener: Option<UserNotificationListener> = None;
        let mut asked = false;
        let mut seen: HashSet<u32> = HashSet::new();
        let mut primed = false;
        loop {
            discord.store(apps_recording().iter().any(|a| a.contains("discord")), Ordering::Relaxed);

            if !mirror.load(Ordering::Relaxed) {
                asked = false; // выключили и включили снова — спросим разрешение ещё раз
            } else {
                if listener.is_none() && !asked {
                    asked = true;
                    let res = UserNotificationListener::Current().and_then(|l| Ok((l.RequestAccessAsync()?.get()?, l)));
                    match res {
                        Ok((UserNotificationListenerAccessStatus::Allowed, l)) => {
                            access.store(1, Ordering::Relaxed);
                            listener = Some(l);
                        }
                        Ok(_) => access.store(2, Ordering::Relaxed),
                        Err(_) => access.store(3, Ordering::Relaxed),
                    }
                }
                if let Some(l) = &listener {
                    if let Ok(list) = l.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()) {
                        for n in list {
                            let Ok(id) = n.Id() else { continue };
                            // первый проход только запоминает то, что уже лежит в центре уведомлений
                            if !seen.insert(id) || !primed {
                                continue;
                            }
                            if let Some(notice) = read(&n) {
                                report(notice);
                            }
                        }
                        primed = true;
                        if seen.len() > 800 {
                            seen.clear();
                            primed = false;
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
    live
}

fn read(n: &windows::UI::Notifications::UserNotification) -> Option<Notice> {
    let app = n.AppInfo().ok()?.DisplayInfo().ok()?.DisplayName().ok()?.to_string();
    let low = app.to_lowercase();
    if low.contains("островок") || !MESSENGERS.iter().any(|m| low.contains(m)) {
        return None;
    }
    let binding = n.Notification().ok()?.Visual().ok()?.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()?;
    let mut texts = Vec::new();
    for t in binding.GetTextElements().ok()? {
        if let Ok(s) = t.Text() {
            let s = s.to_string();
            if !s.trim().is_empty() {
                texts.push(s);
            }
        }
    }
    let title = texts.first().cloned().unwrap_or_else(|| app.clone());
    let body = texts.iter().skip(1).cloned().collect::<Vec<_>>().join(" · ");
    Some(Notice { app, title, body })
}

/// Имена программ (в нижнем регистре), которые прямо сейчас записывают звук с микрофона.
fn apps_recording() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return names };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications) else { return names };
        let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { return names };
        let Ok(sessions) = manager.GetSessionEnumerator() else { return names };
        for i in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(session) = sessions.GetSession(i) else { continue };
            if session.GetState().ok() != Some(AudioSessionStateActive) {
                continue;
            }
            let Ok(pid) = session.cast::<IAudioSessionControl2>().and_then(|s| s.GetProcessId()) else { continue };
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { continue };
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                if let Some(name) = path.rsplit('\\').next() {
                    names.push(name.to_lowercase());
                }
            }
            let _ = CloseHandle(process);
        }
    }
    names
}
