//! Где стоит окно острова: пружина размера, округление к пикселям экрана, магнит и перетаскивание.
//! Только чистые функции — их легко проверять тестами.

use eframe::egui::{vec2, Rect, Vec2};

/// Порог, после которого нажатие становится перетаскиванием (в точках экрана).
pub const DRAG_START: f32 = 4.0;
/// Радиус магнита к верхнему краю и к центру экрана.
pub const SNAP: f32 = 24.0;

/// Один шаг пружины размера. Возвращает новый размер, скорость и «пришла ли к цели».
/// Около цели пружина останавливается ровно на цели, а не дрожит на долях пикселя.
pub fn spring(size: Vec2, vel: Vec2, target: Vec2, dt: f32) -> (Vec2, Vec2, bool) {
    let dt = dt.clamp(0.0, 0.02);
    let mut vel = vel + ((target - size) * 340.0 - vel * 31.0) * dt;
    let mut size = size + vel * dt;
    let settled = (target - size).length() < 0.3 && vel.length() < 4.0;
    if settled {
        size = target;
        vel = Vec2::ZERO;
    }
    (size, vel, settled)
}

/// Прямоугольник окна в пикселях экрана: x, y, ширина, высота.
/// Края округляются по отдельности, поэтому ширина не «дышит» от дробной позиции.
pub fn phys_rect(base: [f32; 2], win: Rect, ppp: f32) -> [i32; 4] {
    let l = (base[0] + win.left() * ppp).round() as i32;
    let t = (base[1] + win.top() * ppp).round() as i32;
    let r = (base[0] + win.right() * ppp).round() as i32;
    let b = (base[1] + win.bottom() * ppp).round() as i32;
    [l, t, (r - l).max(1), (b - t).max(1)]
}

/// Перешло ли нажатие в перетаскивание.
pub fn drag_started(press: [f32; 2], now: [f32; 2], ppp: f32) -> bool {
    vec2(now[0] - press[0], now[1] - press[1]).length() > DRAG_START * ppp
}

/// Начало «поля» острова (в пикселях экрана) так, чтобы его кусок `part` (в точках поля) не вылезал за монитор `mon` [l, t, r, b].
pub fn clamp_into(base: [f32; 2], part: Rect, ppp: f32, mon: [i32; 4]) -> [f32; 2] {
    let fit = |v: f32, lo: f32, hi: f32| if hi < lo { lo } else { v.clamp(lo, hi) };
    let x = fit(base[0], mon[0] as f32 - part.left() * ppp, mon[2] as f32 - part.right() * ppp);
    let y = fit(base[1], mon[1] as f32 - part.top() * ppp, mon[3] as f32 - part.bottom() * ppp);
    [x.round(), y.round()]
}

/// Магнит при отпускании: поле шириной `field_w` точек прилипает к верху и к центру монитора.
pub fn snap(base: [f32; 2], field_w: f32, ppp: f32, mon: [i32; 4]) -> [f32; 2] {
    let r = SNAP * ppp;
    let center = (mon[0] + mon[2]) as f32 / 2.0 - field_w * ppp / 2.0;
    let x = if (base[0] - center).abs() <= r { center } else { base[0] };
    let y = if (base[1] - mon[1] as f32).abs() <= r { mon[1] as f32 } else { base[1] };
    [x.round(), y.round()]
}

/// Место по умолчанию: по центру сверху.
pub fn home(field_w: f32, ppp: f32, mon: [i32; 4]) -> [f32; 2] {
    [((mon[0] + mon[2]) as f32 / 2.0 - field_w * ppp / 2.0).round(), mon[1] as f32]
}

/// Прятать ли остров: активное окно `win` стоит на мониторе `win_mon`, остров — на `island_mon`.
/// Прячемся, только если это тот же монитор и окно заняло его целиком (или система говорит, что идёт игра).
pub fn hides_island(win: [i32; 4], win_mon: [i32; 4], island_mon: [i32; 4], game: bool) -> bool {
    win_mon == island_mon && (win == win_mon || game)
}

/// Ключ монитора для настроек.
pub fn mon_key(mon: [i32; 4]) -> String {
    format!("{},{},{},{}", mon[0], mon[1], mon[2], mon[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    #[test]
    fn spring_comes_to_rest_exactly() {
        let (mut s, mut v) = (vec2(120.0, 36.0), Vec2::ZERO);
        let t = vec2(520.0, 300.0);
        let mut done = false;
        for _ in 0..400 {
            let r = spring(s, v, t, 1.0 / 60.0);
            (s, v, done) = r;
            if done {
                break;
            }
        }
        assert!(done);
        assert_eq!(s, t);
        assert_eq!(v, Vec2::ZERO);
        // и дальше стоит на месте
        assert_eq!(spring(s, v, t, 1.0 / 60.0), (t, Vec2::ZERO, true));
    }

    #[test]
    fn spring_big_dt_is_stable() {
        let r = spring(vec2(10.0, 10.0), Vec2::ZERO, vec2(500.0, 500.0), 1.0);
        assert!(r.0.x.is_finite() && r.0.x < 600.0);
    }

    #[test]
    fn phys_rect_width_stable_with_fractional_pos() {
        let win = Rect::from_min_size(pos2(10.3, 0.0), vec2(200.0, 50.0));
        let a = phys_rect([100.0, 0.0], win, 1.25);
        let b = phys_rect([100.0, 0.0], win.translate(vec2(0.2, 0.0)), 1.25);
        assert_eq!(a[2], 250);
        assert_eq!(b[2], 250);
        assert_eq!(phys_rect([0.0, 0.0], win, 1.25), phys_rect([0.0, 0.0], win, 1.25));
    }

    #[test]
    fn drag_threshold() {
        assert!(!drag_started([0.0, 0.0], [3.0, 2.0], 1.0));
        assert!(drag_started([0.0, 0.0], [5.0, 0.0], 1.0));
        assert!(!drag_started([0.0, 0.0], [5.0, 0.0], 1.5));
    }

    #[test]
    fn snap_to_top_and_center() {
        let mon = [0, 0, 1920, 1080];
        assert_eq!(snap([580.0, 15.0], 780.0, 1.0, mon), [570.0, 0.0]);
        assert_eq!(snap([300.0, 200.0], 780.0, 1.0, mon), [300.0, 200.0]);
        // второй монитор справа
        assert_eq!(snap([1920.0 + 575.0, 10.0], 780.0, 1.0, [1920, 0, 3840, 1080]), [2490.0, 0.0]);
    }

    #[test]
    fn clamp_keeps_panel_on_screen() {
        let mon = [0, 0, 1920, 1080];
        let panel = Rect::from_min_size(pos2(130.0, 6.0), vec2(520.0, 400.0));
        let b = clamp_into([1700.0, 900.0], panel, 1.0, mon);
        assert_eq!(b, [1920.0 - 650.0, 1080.0 - 406.0]);
        assert_eq!(clamp_into([-500.0, -50.0], panel, 1.0, mon), [-130.0, -6.0]);
        assert_eq!(clamp_into([100.0, 50.0], panel, 1.0, mon), [100.0, 50.0]);
    }

    #[test]
    fn hides_only_on_island_monitor() {
        let a = [0, 0, 1920, 1080];
        let b = [1920, 0, 4480, 1440];
        // окно на весь экран острова
        assert!(hides_island(a, a, a, false));
        // на весь второй монитор, остров на первом
        assert!(!hides_island(b, b, a, false));
        assert!(!hides_island(b, b, a, true));
        // игра на мониторе острова, окно не на весь экран (оконный режим без рамки с панелью задач)
        assert!(hides_island([0, 0, 1920, 1040], a, a, true));
        assert!(!hides_island([0, 0, 1920, 1040], a, a, false));
        // остров перетащили на второй монитор, игра там
        assert!(hides_island(b, b, b, false));
    }

    #[test]
    fn home_and_key() {
        assert_eq!(home(780.0, 1.0, [0, 0, 1920, 1080]), [570.0, 0.0]);
        assert_eq!(mon_key([1, 2, 3, 4]), "1,2,3,4");
    }
}
