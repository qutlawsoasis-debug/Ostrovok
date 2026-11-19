//! Тихий режим: пока на мониторе острова полноэкранная игра (или видео, презентация), остров почти
//! ничего не делает — не слушает звук, не ходит в сеть, редко рисует и реже опрашивает систему.
//! Флаг общий для всех потоков; здесь же чистые правила «что можно в игре» — их проверяют тесты.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static IN_GAME: AtomicBool = AtomicBool::new(false);

/// Идёт ли сейчас полноэкранное приложение на мониторе острова (ставит интерфейс раз за кадр).
pub fn in_game() -> bool {
    IN_GAME.load(Ordering::Relaxed)
}

/// Возвращает true, если значение изменилось.
pub fn set_in_game(on: bool) -> bool {
    IN_GAME.swap(on, Ordering::Relaxed) != on
}

/// В игре окно перерисовывается не чаще раза в 2 секунды (если пользователь не трогает остров).
pub const GAME_FRAME: Duration = Duration::from_secs(2);

/// Фоновые дела, которые можно отложить.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Job {
    /// проверка и скачивание обновления (сеть)
    UpdateCheck,
    /// погода (сеть)
    Weather,
    /// текст песни (сеть)
    Lyrics,
    /// перечитать ярлыки и приложения (диск, COM)
    Reindex,
    /// загрузка процессора, памяти, сети
    Stats,
    /// картинка из буфера обмена (до десятков МБ)
    ClipImage,
    /// снимок экрана под островом для стекла
    Lens,
    /// опрос Bluetooth через PowerShell
    Bluetooth,
    /// захват звука для спектров
    SoundCapture,
    /// текст из буфера обмена — дёшево, история не должна терять скопированное в игре
    ClipText,
    /// напоминания и таймеры — должны срабатывать и в игре
    Reminders,
}

/// Можно ли делать это сейчас.
pub fn allowed(job: Job, in_game: bool) -> bool {
    !in_game || matches!(job, Job::ClipText | Job::Reminders)
}

/// Фоновые опросы с собственным ритмом.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Src {
    /// live.rs: кто пишет микрофон (звонок), уведомления
    Live,
    /// media.rs: что играет
    Media,
    /// sys::spawn_watch: громкость и раскладка
    Watch,
    /// sys::spawn_downloads: папка «Загрузки» (busy — идёт загрузка)
    Downloads { busy: bool },
}

/// Пауза между опросами.
pub fn poll_every(src: Src, in_game: bool) -> Duration {
    let ms = match (src, in_game) {
        (Src::Live, false) => 2_000,
        (Src::Live, true) => 10_000,
        (Src::Media, false) => 1_000,
        (Src::Media, true) => 5_000,
        (Src::Watch, false) => 120,
        (Src::Watch, true) => 1_000,
        (Src::Downloads { busy: true }, false) => 1_000,
        (Src::Downloads { busy: false }, false) => 3_000,
        (Src::Downloads { .. }, true) => 15_000,
    };
    Duration::from_millis(ms)
}

/// Когда перерисовать: `want` — сколько хотел бы подождать интерфейс (ноль — сразу).
/// В игре без взаимодействия — не чаще GAME_FRAME; если пользователь трогает остров — как просили.
pub fn repaint_after(in_game: bool, interacting: bool, want: Duration) -> Duration {
    if in_game && !interacting {
        want.max(GAME_FRAME)
    } else {
        want
    }
}

/// Слушать ли звук (cpal): только когда открыта страница со спектром и не идёт игра.
pub fn sound_capture(in_game: bool, spectrum_on_screen: bool) -> bool {
    !in_game && spectrum_on_screen
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Job; 11] = [
        Job::UpdateCheck,
        Job::Weather,
        Job::Lyrics,
        Job::Reindex,
        Job::Stats,
        Job::ClipImage,
        Job::Lens,
        Job::Bluetooth,
        Job::SoundCapture,
        Job::ClipText,
        Job::Reminders,
    ];

    #[test]
    fn outside_game_everything_allowed() {
        assert!(ALL.iter().all(|j| allowed(*j, false)));
    }

    #[test]
    fn in_game_only_cheap_and_promised() {
        let ok: Vec<Job> = ALL.iter().copied().filter(|j| allowed(*j, true)).collect();
        assert_eq!(ok, vec![Job::ClipText, Job::Reminders]);
    }

    #[test]
    fn game_polls_are_never_faster() {
        for src in [Src::Live, Src::Media, Src::Watch, Src::Downloads { busy: true }, Src::Downloads { busy: false }] {
            assert!(poll_every(src, true) >= poll_every(src, false), "{src:?}");
            assert!(poll_every(src, true) >= Duration::from_secs(1), "{src:?}");
        }
        assert_eq!(poll_every(Src::Watch, false), Duration::from_millis(120));
    }

    #[test]
    fn repaint_capped_in_game() {
        let f = Duration::from_millis(16);
        assert_eq!(repaint_after(true, false, f), GAME_FRAME);
        assert_eq!(repaint_after(true, false, Duration::ZERO), GAME_FRAME);
        assert_eq!(repaint_after(true, false, Duration::from_secs(5)), Duration::from_secs(5));
        assert_eq!(repaint_after(true, true, f), f);
        assert_eq!(repaint_after(false, false, f), f);
    }

    #[test]
    fn sound_capture_truth_table() {
        assert!(sound_capture(false, true));
        assert!(!sound_capture(false, false));
        assert!(!sound_capture(true, true));
        assert!(!sound_capture(true, false));
    }

    #[test]
    fn flag_reports_change() {
        // тест не трогает чужие потоки: флаг возвращаем как был
        let was = in_game();
        set_in_game(false);
        assert!(set_in_game(true));
        assert!(!set_in_game(true));
        assert!(in_game());
        assert!(set_in_game(false));
        set_in_game(was);
    }
}
