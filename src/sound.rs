//! «Живой звук»: остров слышит то, что играет на компьютере, и раскладывает это на полосы частот
//! для эквалайзера. Звук берётся с устройства вывода (то, что идёт в колонки), никуда не пишется
//! и не отправляется — от него остаются только высоты столбиков.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BANDS: usize = 16;
const N: usize = 1024;

#[derive(Clone, Copy, Default)]
pub struct Frame {
    /// громкость по полосам от низких к высоким, 0…1
    pub bands: [f32; BANDS],
    /// сила низких частот — «удар» для свечения
    pub bass: f32,
}

struct State {
    frame: Frame,
    /// когда в последний раз было слышно что-то громче шороха
    loud: Option<Instant>,
}

#[derive(Clone)]
pub struct Sound {
    state: Arc<Mutex<State>>,
    /// слушать ли сейчас: выключено — поток спит, устройство освобождено
    want: Arc<AtomicBool>,
    thread: Option<std::thread::Thread>,
}

impl Sound {
    /// Поток захвата стартует спящим; слушать начинает после `set_listen(true)`.
    pub fn spawn() -> Self {
        let mut s = Sound { state: Arc::new(Mutex::new(State { frame: Frame::default(), loud: None })), want: Arc::new(AtomicBool::new(false)), thread: None };
        let state = s.state.clone();
        if std::env::var_os("OSTROVOK_SOUND_DEMO").is_some() {
            // для снимков: нарисованный звук, чтобы эквалайзер было видно без музыки
            std::thread::spawn(move || {
                let t0 = Instant::now();
                loop {
                    let t = t0.elapsed().as_secs_f32();
                    let mut f = Frame::default();
                    for (i, b) in f.bands.iter_mut().enumerate() {
                        let x = i as f32;
                        *b = (0.55 + 0.4 * (t * 3.1 + x * 0.9).sin() * (t * 1.3 + x * 0.37).cos()).clamp(0.05, 1.0) * (1.0 - x / 28.0);
                    }
                    f.bass = f.bands[1];
                    *state.lock().unwrap() = State { frame: f, loud: Some(Instant::now()) };
                    std::thread::sleep(Duration::from_millis(30));
                }
            });
            return s;
        }
        let want = s.want.clone();
        let th = std::thread::spawn(move || loop {
            if !want.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_secs(5));
                continue;
            }
            // устройство могли отключить или сменить — тогда открываем заново
            let _ = listen(&state, &want);
            if want.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_secs(3));
            }
        });
        s.thread = Some(th.thread().clone());
        s
    }

    /// Включить или выключить захват. Выключение освобождает устройство почти сразу.
    pub fn set_listen(&self, on: bool) {
        if self.want.swap(on, Ordering::Relaxed) != on {
            if !on {
                let mut st = self.state.lock().unwrap();
                st.loud = None;
                st.frame = Frame::default();
            }
            if let Some(t) = &self.thread {
                t.unpark();
            }
        }
    }

    /// Играет ли что-нибудь прямо сейчас.
    pub fn active(&self) -> bool {
        self.state.lock().unwrap().loud.is_some_and(|t| t.elapsed() < Duration::from_millis(700))
    }

    pub fn frame(&self) -> Frame {
        let st = self.state.lock().unwrap();
        if st.loud.is_some_and(|t| t.elapsed() < Duration::from_millis(700)) {
            st.frame
        } else {
            Frame::default()
        }
    }
}

/// Слушает устройство вывода, пока оно не сменится, не сломается или захват не выключат.
fn listen(state: &Arc<Mutex<State>>, want: &AtomicBool) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("нет устройства вывода")?;
    let name = device.id().map(|i| i.to_string()).unwrap_or_default();
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let rate = config.sample_rate() as u32 as f32;
    let ch = (config.channels() as usize).max(1);
    let broken = Arc::new(AtomicBool::new(false));
    let on_err = {
        let broken = broken.clone();
        move |_e: cpal::Error| broken.store(true, Ordering::Relaxed)
    };
    // на устройстве вывода «входной» поток — это запись того, что на нём играет
    macro_rules! stream {
        ($t:ty, $conv:expr) => {{
            let state = state.clone();
            let mut an = Analyzer::new(rate);
            device.build_input_stream(
                config.clone().into(),
                move |data: &[$t], _: &_| {
                    for f in data.chunks(ch) {
                        let v = f.iter().map(|s| $conv(*s)).sum::<f32>() / f.len() as f32;
                        if let Some((frame, loud)) = an.push(v) {
                            let mut st = state.lock().unwrap();
                            st.frame = frame;
                            if loud {
                                st.loud = Some(Instant::now());
                            }
                        }
                    }
                },
                on_err,
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => stream!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => stream!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::I32 => stream!(i32, |s: i32| s as f32 / 2147483648.0),
        cpal::SampleFormat::U16 => stream!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        _ => return Err("неизвестный формат звука".into()),
    }
    .map_err(|e| e.to_string())?;
    // пока открывали устройство, захват могли выключить — тогда даже не начинаем
    if !want.load(Ordering::Relaxed) {
        return Ok(());
    }
    stream.play().map_err(|e| e.to_string())?;
    let mut tick = 0u32;
    loop {
        // unpark из set_listen прерывает ожидание сразу
        std::thread::park_timeout(Duration::from_millis(250));
        if !want.load(Ordering::Relaxed) {
            // выключили (игра, панель закрыта): stream уничтожается — устройство и аудиопоток освобождены
            drop(stream);
            let mut st = state.lock().unwrap();
            st.loud = None;
            st.frame = Frame::default();
            return Ok(());
        }
        tick = tick.wrapping_add(1);
        if tick % 16 != 0 {
            continue;
        }
        if broken.load(Ordering::Relaxed) {
            return Err("поток звука оборвался".into());
        }
        // переключили наушники или колонки
        let now = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string()).unwrap_or_default();
        if now != name {
            return Ok(());
        }
    }
}

/// Копит звук и раз в полкадра раскладывает его на полосы частот.
struct Analyzer {
    buf: Vec<f32>,
    window: Vec<f32>,
    /// границы полос в номерах частотных отсчётов
    edges: [usize; BANDS + 1],
    out: [f32; BANDS],
}

impl Analyzer {
    fn new(rate: f32) -> Self {
        let window = (0..N).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (N - 1) as f32).cos()).collect();
        // полосы растут по частоте как ноты: от 50 Гц до 14 кГц
        let mut edges = [0usize; BANDS + 1];
        for (i, e) in edges.iter_mut().enumerate() {
            let hz = 50.0 * (14000.0f32 / 50.0).powf(i as f32 / BANDS as f32);
            *e = ((hz / rate * N as f32) as usize).clamp(1, N / 2 - 1);
        }
        for i in 1..=BANDS {
            if edges[i] <= edges[i - 1] {
                edges[i] = (edges[i - 1] + 1).min(N / 2);
            }
        }
        Analyzer { buf: Vec::with_capacity(N), window, edges, out: [0.0; BANDS] }
    }

    /// Возвращает новый кадр и признак «слышно», когда набралось достаточно звука.
    fn push(&mut self, v: f32) -> Option<(Frame, bool)> {
        self.buf.push(v);
        if self.buf.len() < N {
            return None;
        }
        let mut re: Vec<f32> = self.buf.iter().zip(&self.window).map(|(s, w)| s * w).collect();
        let mut im = vec![0.0f32; N];
        let peak = self.buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        // половина окна остаётся — кадры идут вдвое чаще
        self.buf.drain(..N / 2);
        fft(&mut re, &mut im);
        for b in 0..BANDS {
            let (lo, hi) = (self.edges[b], self.edges[b + 1].max(self.edges[b] + 1));
            let mag = (lo..hi).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).fold(0.0f32, f32::max) / (N as f32 / 4.0);
            // в децибелах: от −62 до −8 дБ растягиваем на 0…1; высоким немного помогаем — они тише
            let db = 20.0 * mag.max(1e-6).log10() + b as f32 * 0.9;
            let v = ((db + 62.0) / 54.0).clamp(0.0, 1.0);
            // вверх столбик прыгает сразу, вниз оседает плавно
            self.out[b] = if v > self.out[b] { v } else { self.out[b] * 0.86 + v * 0.14 };
        }
        let bass = (self.out[0] + self.out[1] + self.out[2]) / 3.0;
        Some((Frame { bands: self.out, bass }, peak > 0.0015))
    }
}

/// Быстрое преобразование Фурье на месте, длина — степень двойки.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Чистый тон должен попасть в свою полосу, а не в соседние.
    #[test]
    fn tone_lands_in_its_band() {
        let rate = 48000.0;
        for hz in [100.0f32, 1000.0, 8000.0] {
            let mut an = Analyzer::new(rate);
            let mut last = None;
            for i in 0..N * 3 {
                if let Some(f) = an.push(0.5 * (std::f32::consts::TAU * hz * i as f32 / rate).sin()) {
                    last = Some(f);
                }
            }
            let (frame, loud) = last.unwrap();
            assert!(loud);
            let top = (0..BANDS).max_by(|a, b| frame.bands[*a].total_cmp(&frame.bands[*b])).unwrap();
            let expect = ((hz / 50.0).ln() / (14000.0f32 / 50.0).ln() * BANDS as f32) as usize;
            assert!(top.abs_diff(expect) <= 1, "{hz} Гц: полоса {top}, ждали {expect}");
            assert!(frame.bands[top] > 0.7, "{hz} Гц: {}", frame.bands[top]);
        }
    }

    #[test]
    fn silence_is_quiet() {
        let mut an = Analyzer::new(48000.0);
        let mut last = None;
        for _ in 0..N * 2 {
            if let Some(f) = an.push(0.0) {
                last = Some(f);
            }
        }
        let (frame, loud) = last.unwrap();
        assert!(!loud && frame.bands.iter().all(|b| *b < 0.01));
    }
}
