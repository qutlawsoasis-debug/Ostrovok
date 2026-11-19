//! Живой эквалайзер в пилюле: столбики по пиковому уровню устройства вывода (IAudioMeterInformation).
//! Сам звук не захватывается — Windows отдаёт только пики по каналам. Здесь только чистые функции.

use std::time::Duration;

/// Сколько столбиков.
pub const N: usize = 7;
/// Ширина столбика и шаг между ними (точки).
pub const BAR_W: f32 = 3.0;
pub const STEP: f32 = 4.0;
/// Ширина всего эквалайзера в пилюле.
pub const EQ_W: f32 = STEP * (N as f32 - 1.0) + BAR_W;
/// Опрос уровня не чаще 30 раз в секунду.
pub const POLL: Duration = Duration::from_millis(33);
/// Пока тихо — проверка «не заиграло ли что-нибудь» раз в полсекунды.
pub const PROBE: Duration = Duration::from_millis(500);

/// Слышно ли что-то громче шороха (около −56 дБ).
pub fn heard(peak: f32) -> bool {
    peak.is_finite() && peak > 0.0015
}

/// Пик 0…1 (линейный) → высота 0…1: от −48 дБ до 0 дБ, тишина и мусор — ноль.
pub fn level(peak: f32) -> f32 {
    if !peak.is_finite() || peak <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * peak.min(1.0).log10();
    ((db + 48.0) / 48.0).clamp(0.0, 1.0)
}

/// Плавное движение к цели: вверх быстро (~40 мс), вниз медленно (~250 мс).
pub fn smooth(prev: f32, target: f32, dt: f32) -> f32 {
    let dt = if dt.is_finite() { dt.clamp(0.0, 0.1) } else { 0.0 };
    let target = if target.is_finite() { target.clamp(0.0, 1.0) } else { 0.0 };
    let tau = if target > prev { 0.04 } else { 0.25 };
    let v = prev + (target - prev) * (1.0 - (-dt / tau).exp());
    if (v - target).abs() < 0.002 {
        target
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Раскладка по столбикам: левый канал слева, правый справа, середина чуть выше краёв,
/// у каждого столбика своё лёгкое покачивание, чтобы ряд не прыгал одной стеной.
pub fn spread(level: f32, lr: [f32; 2], t: f32) -> [f32; N] {
    let mut out = [0.0; N];
    for (i, v) in out.iter_mut().enumerate() {
        let pos = i as f32 / (N as f32 - 1.0);
        let ch = lr[0] * (1.0 - pos) + lr[1] * pos;
        let shape = 0.72 + 0.28 * (std::f32::consts::PI * pos).sin();
        let wobble = 0.78 + 0.22 * (t * (6.0 + i as f32 * 1.9) + i as f32 * 1.3).sin();
        let x = (0.45 * level + 0.55 * ch) * shape * wobble;
        *v = if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 };
    }
    out
}

/// Левый край столбика `i` от начала эквалайзера.
pub fn bar_x(i: usize) -> f32 {
    i as f32 * STEP
}

/// Высота столбика в точках.
pub fn height(v: f32) -> f32 {
    3.0 + 13.0 * v.clamp(0.0, 1.0)
}

/// Пора ли спросить уровень: пилюля видна, звук идёт, поверх нет полноэкранного приложения, прошло ≥ POLL.
pub fn should_poll(visible: bool, playing: bool, fullscreen: bool, since: Duration) -> bool {
    visible && playing && !fullscreen && since >= POLL
}

/// Пики для снимка без звука (OSTROVOK_EQ_DEMO): правдоподобная «музыка» по времени.
pub fn demo_peak(t: f32) -> (f32, [f32; 2]) {
    let beat = (0.5 + 0.5 * (t * 7.3).sin()).powf(3.0);
    let l = 0.25 + 0.55 * beat + 0.1 * (t * 3.1).sin().abs();
    let r = 0.25 + 0.45 * beat + 0.15 * (t * 4.7 + 1.0).sin().abs();
    (l.max(r).min(1.0), [l.min(1.0), r.min(1.0)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_and_garbage_are_floor() {
        assert_eq!(level(0.0), 0.0);
        assert_eq!(level(-1.0), 0.0);
        assert_eq!(level(f32::NAN), 0.0);
        assert_eq!(level(f32::INFINITY), 0.0);
        assert_eq!(level(1.0), 1.0);
        assert_eq!(level(5.0), 1.0);
        // −24 дБ — середина
        assert!((level(0.0631) - 0.5).abs() < 0.01);
        // очень тихо — у пола
        assert_eq!(level(0.001), 0.0);
    }

    #[test]
    fn rises_faster_than_falls() {
        let up = smooth(0.0, 1.0, 1.0 / 30.0);
        let down = 1.0 - smooth(1.0, 0.0, 1.0 / 30.0);
        assert!(up > down * 2.0, "up {up} down {down}");
    }

    #[test]
    fn smooth_converges_and_stops() {
        let mut v = 1.0;
        for _ in 0..200 {
            v = smooth(v, 0.0, 1.0 / 30.0);
        }
        assert_eq!(v, 0.0);
        let mut v = 0.0;
        for _ in 0..60 {
            v = smooth(v, 0.6, 1.0 / 30.0);
        }
        assert_eq!(v, 0.6);
        // большой шаг и мусор не ломают
        assert!(smooth(0.0, 1.0, 10.0) <= 1.0);
        assert_eq!(smooth(0.3, f32::NAN, 0.03), smooth(0.3, 0.0, 0.03));
        assert_eq!(smooth(0.3, 0.5, f32::NAN), 0.3);
    }

    #[test]
    fn spread_stays_in_range_and_follows_channels() {
        for k in 0..100 {
            let t = k as f32 * 0.07;
            for v in spread(1.0, [1.0, 1.0], t) {
                assert!((0.0..=1.0).contains(&v));
            }
            assert_eq!(spread(0.0, [0.0, 0.0], t), [0.0; N]);
        }
        // слышен только левый канал — левый край выше правого
        let s = spread(0.5, [1.0, 0.0], 0.0);
        assert!(s[0] > s[N - 1]);
        let nan = spread(f32::NAN, [0.5, 0.5], 0.0);
        assert!(nan.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn bars_fit_width() {
        assert_eq!(bar_x(0), 0.0);
        assert!((bar_x(N - 1) + BAR_W - EQ_W).abs() < 1e-6);
        assert!(EQ_W <= 28.0);
        assert_eq!(height(0.0), 3.0);
        assert_eq!(height(2.0), 16.0);
    }

    #[test]
    fn poll_rate_and_conditions() {
        let ok = Duration::from_millis(40);
        assert!(should_poll(true, true, false, ok));
        assert!(!should_poll(true, true, false, Duration::from_millis(10)));
        assert!(!should_poll(false, true, false, ok));
        assert!(!should_poll(true, false, false, ok));
        assert!(!should_poll(true, true, true, ok));
    }

    #[test]
    fn heard_threshold() {
        assert!(!heard(0.0));
        assert!(!heard(0.001));
        assert!(!heard(f32::NAN));
        assert!(heard(0.01));
        assert!(PROBE > POLL);
    }

    #[test]
    fn demo_in_range() {
        for k in 0..200 {
            let (p, [l, r]) = demo_peak(k as f32 * 0.05);
            assert!((0.0..=1.0).contains(&p) && (0.0..=1.0).contains(&l) && (0.0..=1.0).contains(&r));
        }
    }
}
