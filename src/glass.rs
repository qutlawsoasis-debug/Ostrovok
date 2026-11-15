//! Жидкое стекло: снимок того, что на экране под островом, лёгкое размытие и «линза» у края.
//!
//! Алгоритм преломления перенесён из Atoll (Fuwni/dynamic-island-windows, MainWindow.LiquidGlass.cs,
//! лицензия MIT, © 2026 mykolaichechelnytskyi): у кромки картинка стягивается к краю, как под толстым
//! выпуклым стеклом, а красный преломляется чуть сильнее синего — получается тонкая радужная кайма.
//! Снимок берётся редко (раз в ~0,7 с) и в отдельном потоке, поэтому почти ничего не стоит.

use std::sync::mpsc;

pub struct Req {
    /// область экрана в пикселях
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// остров внутри этой области: левый верхний угол, размер, радиус скругления (в пикселях)
    pub ix: f64,
    pub iy: f64,
    pub iw: f64,
    pub ih: f64,
    pub rr: f64,
    /// та же область в координатах поля острова (точки) — возвращается вместе с кадром
    pub cap: [f32; 4],
}

pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    pub cap: [f32; 4],
}

pub fn spawn(ctx: eframe::egui::Context) -> (mpsc::Sender<Req>, mpsc::Receiver<Frame>) {
    let (req_tx, req_rx) = mpsc::channel::<Req>();
    let (out_tx, out_rx) = mpsc::channel::<Frame>();
    std::thread::spawn(move || {
        let mut scratch = Vec::new();
        while let Ok(mut req) = req_rx.recv() {
            // если запросов накопилось несколько, нужен только последний
            while let Ok(newer) = req_rx.try_recv() {
                req = newer;
            }
            let Some(mut px) = crate::sys::grab_bgrx(req.x, req.y, req.w, req.h) else { continue };
            let (w, h) = (req.w as usize, req.h as usize);
            blur(&mut px, &mut scratch, w, h, 3, 2);
            let mut out = refract(&px, w, h, req.ix, req.iy, req.iw, req.ih, req.rr);
            for c in out.chunks_exact_mut(4) {
                c.swap(0, 2); // BGRX -> RGBA
                c[3] = 255;
            }
            if let Ok(path) = std::env::var("OSTROVOK_GLASS_DUMP") {
                let _ = image::save_buffer(&path, &out, w as u32, h as u32, image::ExtendedColorType::Rgba8);
            }
            if out_tx.send(Frame { w, h, rgba: out, cap: req.cap }).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    (req_tx, out_rx)
}

/// Быстрое размытие: несколько проходов усреднения по горизонтали и вертикали.
fn blur(px: &mut [u8], tmp: &mut Vec<u8>, w: usize, h: usize, r: usize, passes: usize) {
    tmp.resize(px.len(), 0);
    let win = (2 * r + 1) as u32;
    for _ in 0..passes {
        for y in 0..h {
            for c in 0..3 {
                let row = y * w * 4 + c;
                let mut sum: u32 = (0..=2 * r).map(|k| px[row + k.saturating_sub(r).min(w - 1) * 4] as u32).sum();
                for x in 0..w {
                    tmp[row + x * 4] = (sum / win) as u8;
                    let add = (x + r + 1).min(w - 1);
                    let sub = x.saturating_sub(r);
                    sum = sum + px[row + add * 4] as u32 - px[row + sub * 4] as u32;
                }
            }
        }
        for x in 0..w {
            for c in 0..3 {
                let col = x * 4 + c;
                let mut sum: u32 = (0..=2 * r).map(|k| tmp[col + k.saturating_sub(r).min(h - 1) * w * 4] as u32).sum();
                for y in 0..h {
                    px[col + y * w * 4] = (sum / win) as u8;
                    let add = (y + r + 1).min(h - 1);
                    let sub = y.saturating_sub(r);
                    sum = sum + tmp[col + add * w * 4] as u32 - tmp[col + sub * w * 4] as u32;
                }
            }
        }
    }
}

fn sample(src: &[u8], w: usize, h: usize, x: f64, y: f64, channel: usize) -> u8 {
    let ix = (x as isize).clamp(0, w as isize - 1) as usize;
    let iy = (y as isize).clamp(0, h as isize - 1) as usize;
    src[(iy * w + ix) * 4 + channel]
}

/// Линза: src — снимок BGRX w×h, остров — скруглённый прямоугольник (ix, iy, iw, ih, rr) внутри него.
#[allow(clippy::too_many_arguments)]
fn refract(src: &[u8], w: usize, h: usize, ix: f64, iy: f64, iw: f64, ih: f64, rr: f64) -> Vec<u8> {
    let mut dst = src.to_vec();
    let (cx, cy, bx, by) = (ix + iw / 2.0, iy + ih / 2.0, iw / 2.0, ih / 2.0);
    let rr = rr.min(bx.min(by));
    let band = (ih * 0.42).min(rr * 1.2 + 3.0).max(3.0); // как далеко от кромки стекло искривляет картинку
    let push = band * 0.9; // насколько картинка стягивается к кромке

    let (y0, y1) = ((iy.max(0.0)) as usize, ((iy + ih).ceil() as usize).min(h));
    let (x0, x1) = ((ix.max(0.0)) as usize, ((ix + iw).ceil() as usize).min(w));
    for y in y0..y1 {
        let py = y as f64 + 0.5;
        let dy = py - cy;
        let qy = dy.abs() - (by - rr);
        for x in x0..x1 {
            let px = x as f64 + 0.5;
            let dx = px - cx;
            let qx = dx.abs() - (bx - rr);
            let (ox, oy) = (qx.max(0.0), qy.max(0.0));
            let d = (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rr; // меньше нуля — внутри острова
            if d >= 0.0 {
                continue;
            }
            let i = (y * w + x) * 4;
            let t = -d / band;
            let (nx, ny) = if qx > 0.0 && qy > 0.0 {
                let l = (qx * qx + qy * qy).sqrt();
                (dx.signum() * qx / l, dy.signum() * qy / l)
            } else if qx > qy {
                (dx.signum(), 0.0)
            } else {
                (0.0, dy.signum())
            };
            if t < 1.0 {
                let f = (1.0 - t) * (1.0 - t);
                let off = f * push;
                dst[i + 2] = sample(src, w, h, px - nx * off * 1.12, py - ny * off * 1.12, 2);
                dst[i + 1] = sample(src, w, h, px - nx * off, py - ny * off, 1);
                dst[i] = sample(src, w, h, px - nx * off * 0.88, py - ny * off * 0.88, 0);
                // свет собирается в кромке
                let glow = (f * 18.0) as u16;
                for c in 0..3 {
                    dst[i + c] = (dst[i + c] as u16 + glow).min(255) as u8;
                }
            }
            // под стеклом цвета чуть насыщеннее
            let (r, g, b) = (dst[i + 2] as i32, dst[i + 1] as i32, dst[i] as i32);
            let gray = (r * 77 + g * 151 + b * 28) >> 8;
            dst[i + 2] = (gray + (r - gray) * 5 / 4).clamp(0, 255) as u8;
            dst[i + 1] = (gray + (g - gray) * 5 / 4).clamp(0, 255) as u8;
            dst[i] = (gray + (b - gray) * 5 / 4).clamp(0, 255) as u8;
        }
    }
    dst
}
