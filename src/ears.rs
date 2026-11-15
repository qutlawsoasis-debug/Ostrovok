//! Слух Джарвиса: запись фразы с микрофона и распознавание речи локальной моделью Whisper (whisper.cpp).
//! Микрофон включается только на время одной фразы; звук никуда не отправляется.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const NO_WINDOW: u32 = 0x0800_0000;
/// сколько ждать начала речи
const WAIT_SPEECH: Duration = Duration::from_secs(7);
/// тишина после фразы, по которой запись заканчивается
const SILENCE: Duration = Duration::from_millis(750);
const MAX_PHRASE: Duration = Duration::from_secs(25);

pub enum Heard {
    /// громкость микрофона 0…1 — для полоски на странице
    Level(f32),
    /// фраза записана, идёт распознавание
    Thinking,
    /// текст, сколько секунд ушло на распознавание и каким движком
    Text(String, f32, &'static str),
    Error(String),
    /// фраза, услышанная в режиме постоянного прослушивания
    Phrase(String),
}

#[derive(Clone)]
pub struct Ears {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    /// постоянно работающий распознаватель: процесс, порт, время последнего обращения
    server: Arc<std::sync::Mutex<Option<(std::process::Child, u16, Instant, isize)>>>,
    starting: Arc<std::sync::Mutex<()>>,
    watching: Arc<AtomicBool>,
}

impl Ears {
    pub fn new(dir: PathBuf) -> Self {
        let e = Ears { dir, stop: Arc::new(AtomicBool::new(false)), server: Default::default(), starting: Default::default(), watching: Default::default() };
        // распознаватель выгружается, если им пять минут не пользовались
        let server = e.server.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(15));
            let mut s = server.lock().unwrap();
            if s.as_ref().is_some_and(|s| s.2.elapsed() > Duration::from_secs(300)) {
                if let Some(mut old) = s.take() {
                    let _ = old.0.kill();
                }
            }
        });
        e
    }

    /// Поднимает распознаватель заранее, чтобы модель не грузилась на каждую фразу.
    pub fn warm(&self) {
        let me = self.clone();
        std::thread::spawn(move || {
            let _ = me.server_port();
        });
    }

    pub fn stop_server(&self) {
        if let Some(mut s) = self.server.lock().unwrap().take() {
            let _ = s.0.kill();
        }
    }

    fn server_port(&self) -> Option<u16> {
        let _one = self.starting.lock().unwrap();
        {
            let mut s = self.server.lock().unwrap();
            if let Some(cur) = s.as_mut() {
                if cur.0.try_wait().ok().flatten().is_none() {
                    cur.2 = Instant::now();
                    return Some(cur.1);
                }
            }
            *s = None;
        }
        let (exe, model, tokens) = self.giga()?;
        let exe = exe.with_file_name("sherpa-onnx-offline-websocket-server.exe");
        if !exe.exists() {
            return None;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).ok()?.port();
        let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6));
        let mut child = Command::new(&exe)
            .arg(format!("--port={port}"))
            .arg(format!("--nemo-ctc-model={}", model.display()))
            .arg(format!("--tokens={}", tokens.display()))
            .arg(format!("--num-threads={threads}"))
            .args(["--num-work-threads=1", "--num-io-threads=1", "--max-utterance-length=40"])
            .current_dir(exe.parent()?)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .spawn()
            .ok()?;
        let job = crate::jarvis::tie_to_us(&child);
        let began = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(120));
            if child.try_wait().ok().flatten().is_some() || began.elapsed() > Duration::from_secs(25) {
                let _ = child.kill();
                return None;
            }
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
        }
        *self.server.lock().unwrap() = Some((child, port, Instant::now(), job));
        Some(port)
    }

    /// Распознавание через работающий сервер: модель уже в памяти, ответ за доли секунды.
    fn transcribe_server(&self, samples: &[f32]) -> Option<String> {
        use tungstenite::Message;
        let port = self.server_port()?;
        let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).ok()?;
        let mut buf: Vec<u8> = Vec::with_capacity(8 + samples.len() * 4);
        buf.extend(16000i32.to_le_bytes());
        buf.extend(((samples.len() * 4) as i32).to_le_bytes());
        for s in samples {
            buf.extend(s.to_le_bytes());
        }
        for part in buf.chunks(10240) {
            ws.send(Message::Binary(part.to_vec().into())).ok()?;
        }
        let text = loop {
            match ws.read().ok()? {
                Message::Text(t) => break t.to_string(),
                Message::Close(_) => return None,
                _ => {}
            }
        };
        let _ = ws.send(Message::Text("Done".into()));
        let _ = ws.close(None);
        Some(serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["text"].as_str().map(str::to_string)).unwrap_or(text))
    }

    fn exe(&self) -> Option<PathBuf> {
        [self.dir.join("ears").join("Release").join("whisper-cli.exe"), self.dir.join("ears").join("whisper-cli.exe")].into_iter().find(|p| p.exists())
    }

    fn model(&self) -> PathBuf {
        self.dir.join("models").join("whisper.bin")
    }

    /// Движок и модель GigaAM — точное и быстрое распознавание русской речи.
    fn giga(&self) -> Option<(PathBuf, PathBuf, PathBuf)> {
        let base = self.dir.join("stt");
        let exe = base.join("engine").join("bin").join("sherpa-onnx-offline.exe");
        let model = base.join("model").join("model.int8.onnx");
        let tokens = base.join("model").join("tokens.txt");
        (exe.exists() && model.exists() && tokens.exists()).then_some((exe, model, tokens))
    }

    pub fn ready(&self) -> bool {
        self.giga().is_some() || (self.exe().is_some() && self.model().exists())
    }

    fn transcribe_giga(&self, wav: &std::path::Path) -> Option<String> {
        let (exe, model, tokens) = self.giga()?;
        let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).clamp(2, 6));
        let out = Command::new(&exe)
            .arg(format!("--nemo-ctc-model={}", model.display()))
            .arg(format!("--tokens={}", tokens.display()))
            .arg(format!("--num-threads={threads}"))
            .arg(wav)
            .current_dir(exe.parent()?)
            .stdin(Stdio::null())
            .creation_flags(NO_WINDOW)
            .output()
            .ok()?;
        // итог печатается строкой JSON с полем text — в обычный вывод или в поток ошибок
        let all = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        all.lines().filter_map(|l| l.find('{').map(|i| &l[i..])).filter_map(|j| serde_json::from_str::<serde_json::Value>(j).ok()).find_map(|v| v["text"].as_str().map(str::to_string))
    }

    /// Закончить запись сейчас и распознать то, что уже сказано.
    pub fn finish(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Слушает одну фразу: ждёт речь, пишет до паузы, распознаёт. О ходе сообщает через emit.
    pub fn listen(&self, emit: impl Fn(Heard) + Send + 'static) {
        let me = self.clone();
        me.stop.store(false, Ordering::Relaxed);
        std::thread::spawn(move || match me.record(&emit) {
            Ok(samples) => {
                emit(Heard::Thinking);
                emit(match me.transcribe(&samples) {
                    Ok((t, secs, engine)) => Heard::Text(t, secs, engine),
                    Err(e) => Heard::Error(e),
                });
            }
            Err(e) => emit(Heard::Error(e)),
        });
    }

    /// Возвращает фразу: моно, 16 кГц.
    fn record(&self, emit: &dyn Fn(Heard)) -> Result<Vec<f32>, String> {
        let device = cpal::default_host().default_input_device().ok_or("Микрофон не найден")?;
        let config = device.default_input_config().map_err(|e| format!("Микрофон недоступен: {e}"))?;
        let rate = config.sample_rate() as u32 as f32;
        let ch = config.channels() as usize;
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let err = |_e: cpal::Error| {};
        // в поток уходит уже сведённое в моно
        macro_rules! stream {
            ($t:ty, $conv:expr) => {{
                let tx = tx.clone();
                device.build_input_stream(
                    config.clone().into(),
                    move |data: &[$t], _: &_| {
                        let mono: Vec<f32> = data.chunks(ch.max(1)).map(|f| f.iter().map(|s| $conv(*s)).sum::<f32>() / f.len() as f32).collect();
                        let _ = tx.send(mono);
                    },
                    err,
                    None,
                )
            }};
        }
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => stream!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => stream!(i16, |s: i16| s as f32 / 32768.0),
            cpal::SampleFormat::I32 => stream!(i32, |s: i32| s as f32 / 2147483648.0),
            cpal::SampleFormat::U16 => stream!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
            other => return Err(format!("Неизвестный формат микрофона: {other}")),
        }
        .map_err(|e| format!("Микрофон не открылся: {e}"))?;
        stream.play().map_err(|e| format!("Микрофон не запустился: {e}"))?;

        let frame = (rate * 0.03) as usize; // 30 мс
        let mut pending: Vec<f32> = Vec::new();
        let mut all: Vec<f32> = Vec::new();
        let (started, mut speech_at, mut last_voice) = (Instant::now(), None::<Instant>, Instant::now());
        let (mut noise, mut frames, mut last_level) = (0.0f32, 0u32, Instant::now());
        loop {
            let chunk = rx.recv_timeout(Duration::from_millis(500)).map_err(|_| "Микрофон молчит: проверьте, что он подключён и не занят")?;
            pending.extend(chunk);
            while pending.len() >= frame {
                let f: Vec<f32> = pending.drain(..frame).collect();
                let rms = (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt();
                frames += 1;
                // первые 0,25 с — оценка фонового шума
                if frames == 1 {
                    noise = rms;
                } else if frames <= 8 {
                    // берём самый тихий кадр: если человек уже говорит, порог не задирается
                    noise = noise.min(rms);
                }
                let thresh = (noise * 3.0).clamp(0.01, 0.04);
                let voiced = frames > 8 && rms > thresh;
                if voiced {
                    last_voice = Instant::now();
                    speech_at.get_or_insert(last_voice);
                }
                all.extend(f);
                // до начала речи храним последние полторы секунды, чтобы не срезать первый слог
                if speech_at.is_none() {
                    let keep = (rate * 1.5) as usize;
                    if all.len() > keep {
                        let cut = all.len() - keep;
                        all.drain(..cut);
                    }
                }
                if last_level.elapsed() > Duration::from_millis(60) {
                    last_level = Instant::now();
                    emit(Heard::Level((rms * 9.0).min(1.0)));
                }
            }
            let stop = self.stop.load(Ordering::Relaxed);
            match speech_at {
                None if stop => return Err("Запись остановлена".into()),
                None if started.elapsed() > WAIT_SPEECH => return Err("Ничего не услышал".into()),
                Some(t) if stop || last_voice.elapsed() > SILENCE || t.elapsed() > MAX_PHRASE => break,
                _ => {}
            }
        }
        drop(stream);
        // приводим к 16 кГц: каждый новый отсчёт — среднее по своему отрезку (иначе высокие частоты портят звук)
        let step = (rate / 16000.0).max(1.0);
        let n = (all.len() as f32 / step) as usize;
        let mut out: Vec<f32> = (0..n)
            .map(|i| {
                let a = (i as f32 * step) as usize;
                let b = (((i + 1) as f32 * step) as usize).clamp(a + 1, all.len());
                all[a..b].iter().sum::<f32>() / (b - a) as f32
            })
            .collect();
        // убираем постоянное смещение и выравниваем громкость: тихий микрофон распознаётся хуже
        let mean = out.iter().sum::<f32>() / out.len().max(1) as f32;
        let peak = out.iter().map(|s| (s - mean).abs()).fold(0.0f32, f32::max);
        let gain = if peak > 0.003 { (0.85 / peak).min(30.0) } else { 1.0 };
        for s in &mut out {
            *s = (*s - mean) * gain;
        }
        Ok(out)
    }

    fn transcribe(&self, samples: &[f32]) -> Result<(String, f32, &'static str), String> {
        let began = Instant::now();
        let wav = std::env::temp_dir().join("ostrovok-heard.wav");
        std::fs::write(&wav, wav_bytes(samples)).map_err(|e| e.to_string())?;
        // последняя запись остаётся в папке Джарвиса — чтобы можно было разобраться, что именно услышал микрофон
        let _ = std::fs::copy(&wav, self.dir.join("last-heard.wav"));
        let giga = match self.transcribe_server(samples) {
            Some(t) => Some((t, "GigaAM")),
            None => self.transcribe_giga(&wav).map(|t| (t, "GigaAM, холодный запуск")),
        };
        if let Some((text, engine)) = giga {
            let _ = std::fs::remove_file(&wav);
            let text = text.trim().to_string();
            if text.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
                return Err("Не разобрал слова".into());
            }
            return Ok((text, began.elapsed().as_secs_f32(), engine));
        }
        let exe = self.exe().ok_or("Не найден движок распознавания речи")?;
        let model = self.model();
        if !model.exists() {
            return Err("Модель распознавания речи ещё не скачана".into());
        }
        let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(2));
        let ctx = ((samples.len() as f32 / 16000.0 / 30.0 * 1500.0) as i32 + 260).clamp(520, 1500);
        let out = Command::new(&exe)
            .arg("-m").arg(&model)
            .arg("-f").arg(&wav)
            .args(["-l", "ru", "-nt", "-np", "-t", &threads.to_string()])
            // короткой фразе не нужно полное 30-секундное окно: так распознавание в разы быстрее
            .args(["-ac", &ctx.to_string(), "-bs", "3", "-bo", "3", "-nf"])
            // подсказка задаёт стиль и написание имени
            .args(["--prompt", "Привет, Джарвис. Джарвис, открой загрузки и сделай музыку тише."])
            .current_dir(exe.parent().unwrap_or(&self.dir))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .output()
            .map_err(|e| format!("Распознавание не запустилось: {e}"))?;
        let _ = std::fs::remove_file(&wav);
        let text = String::from_utf8_lossy(&out.stdout).split_whitespace().collect::<Vec<_>>().join(" ");
        let low = text.to_lowercase();
        // на тишине и шуме модель выдумывает типовые фразы из субтитров
        let junk = ["субтитры", "продолжение следует", "редактор субтитров", "спасибо за просмотр", "dimatorzok"];
        if text.is_empty() || text.starts_with('[') || text.starts_with('(') || text.starts_with('*') || junk.iter().any(|j| low.contains(j)) {
            return Err("Не разобрал слова".into());
        }
        Ok((text, began.elapsed().as_secs_f32(), "Whisper"))
    }
}

/// WAV: моно, 16 кГц, 16 бит.
fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend(b"RIFF");
    b.extend((36 + data).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(16000u32.to_le_bytes());
    b.extend(32000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data.to_le_bytes());
    for s in samples {
        b.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    b
}

/// Открывает микрофон по умолчанию. Звук приходит в канал уже сведённым в моно.
fn open_mic() -> Result<(cpal::Stream, mpsc::Receiver<Vec<f32>>, f32), String> {
    let device = cpal::default_host().default_input_device().ok_or("Микрофон не найден")?;
    let config = device.default_input_config().map_err(|e| format!("Микрофон недоступен: {e}"))?;
    let rate = config.sample_rate() as u32 as f32;
    let ch = (config.channels() as usize).max(1);
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let err = |_e: cpal::Error| {};
    macro_rules! stream {
        ($t:ty, $conv:expr) => {{
            let tx = tx.clone();
            device.build_input_stream(
                config.clone().into(),
                move |data: &[$t], _: &_| {
                    let mono: Vec<f32> = data.chunks(ch).map(|f| f.iter().map(|s| $conv(*s)).sum::<f32>() / f.len() as f32).collect();
                    let _ = tx.send(mono);
                },
                err,
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => stream!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => stream!(i16, |s: i16| s as f32 / 32768.0),
        cpal::SampleFormat::I32 => stream!(i32, |s: i32| s as f32 / 2147483648.0),
        cpal::SampleFormat::U16 => stream!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
        other => return Err(format!("Неизвестный формат микрофона: {other}")),
    }
    .map_err(|e| format!("Микрофон не открылся: {e}"))?;
    stream.play().map_err(|e| format!("Микрофон не запустился: {e}"))?;
    Ok((stream, rx, rate))
}

/// Готовит запись к распознаванию: 16 кГц, без смещения, с выровненной громкостью.
fn prep(all: &[f32], rate: f32) -> Vec<f32> {
    let step = (rate / 16000.0).max(1.0);
    let n = (all.len() as f32 / step) as usize;
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let a = (i as f32 * step) as usize;
            let b = (((i + 1) as f32 * step) as usize).clamp(a + 1, all.len());
            all[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect();
    let mean = out.iter().sum::<f32>() / out.len().max(1) as f32;
    let peak = out.iter().map(|s| (s - mean).abs()).fold(0.0f32, f32::max);
    let gain = if peak > 0.003 { (0.85 / peak).min(30.0) } else { 1.0 };
    for s in &mut out {
        *s = (*s - mean) * gain;
    }
    out
}

impl Ears {
    pub fn watching(&self) -> bool {
        self.watching.load(Ordering::Relaxed)
    }

    pub fn unwatch(&self) {
        self.watching.store(false, Ordering::Relaxed);
    }

    /// Постоянное прослушивание: микрофон открыт, каждая сказанная фраза распознаётся на этом компьютере
    /// и отдаётся в emit. Решать, обращались ли к Джарвису, — дело вызывающего.
    /// busy — счётчик «Джарвис сейчас говорит»: в это время звук отбрасывается, чтобы он не слышал сам себя.
    pub fn watch(&self, busy: Arc<std::sync::atomic::AtomicUsize>, emit: impl Fn(Heard) + Send + 'static) {
        if self.watching.swap(true, Ordering::Relaxed) || self.giga().is_none() {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            if let Err(e) = me.watch_loop(&busy, &emit) {
                emit(Heard::Error(e));
                // не дёргать микрофон без остановки, если его нет: следующая попытка через полминуты
                std::thread::sleep(Duration::from_secs(30));
            }
            me.watching.store(false, Ordering::Relaxed);
        });
    }

    fn watch_loop(&self, busy: &std::sync::atomic::AtomicUsize, emit: &dyn Fn(Heard)) -> Result<(), String> {
        let (_stream, rx, rate) = open_mic()?;
        let _ = self.server_port();
        let frame = (rate * 0.03) as usize; // 30 мс
        let mut pending: Vec<f32> = Vec::new();
        let mut pre: std::collections::VecDeque<f32> = Default::default();
        let mut phrase: Vec<f32> = Vec::new();
        let (mut noise, mut run, mut quiet, mut voiced_frames, mut in_speech) = (0.01f32, 0u32, 0u32, 0u32, false);
        let (mut warm_at, mut hush_until) = (Instant::now(), Instant::now());
        while self.watching.load(Ordering::Relaxed) {
            let chunk = rx.recv_timeout(Duration::from_secs(3)).map_err(|_| "Микрофон перестал отвечать — постоянное прослушивание остановлено")?;
            // пока Джарвис говорит (и чуть после) — не слушаем
            if busy.load(Ordering::Relaxed) > 0 {
                hush_until = Instant::now() + Duration::from_millis(350);
            }
            if Instant::now() < hush_until {
                pending.clear();
                pre.clear();
                phrase.clear();
                (in_speech, run) = (false, 0);
                continue;
            }
            pending.extend(chunk);
            while pending.len() >= frame {
                let f: Vec<f32> = pending.drain(..frame).collect();
                let rms = (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt();
                let voiced = rms > (noise * 3.0).clamp(0.008, 0.05);
                if !in_speech {
                    if !voiced {
                        // фоновый шум оценивается на ходу
                        noise = noise * 0.97 + rms * 0.03;
                    }
                    run = if voiced { run + 1 } else { 0 };
                    pre.extend(f);
                    while pre.len() > (rate * 0.5) as usize {
                        pre.pop_front();
                    }
                    // три кадра подряд — это речь, а не щелчок
                    if run >= 3 {
                        in_speech = true;
                        phrase = pre.drain(..).collect();
                        (quiet, voiced_frames) = (0, run);
                    }
                } else {
                    phrase.extend(f);
                    if voiced {
                        quiet = 0;
                        voiced_frames += 1;
                    } else {
                        quiet += 1;
                    }
                    let long = phrase.len() as f32 > rate * 15.0;
                    // 0,6 с тишины — фраза кончилась
                    if quiet >= 20 || long {
                        (in_speech, run) = (false, 0);
                        let audio = std::mem::take(&mut phrase);
                        if long {
                            // это не речь, а постоянный звук (музыка из колонок): поднимаем порог
                            noise = (noise * 1.6).min(0.02);
                        } else if voiced_frames >= 8 {
                            if let Some(t) = self.transcribe_server(&prep(&audio, rate)) {
                                let t = t.trim();
                                if t.chars().filter(|c| c.is_alphanumeric()).count() >= 2 {
                                    emit(Heard::Phrase(t.to_string()));
                                }
                            }
                        }
                    }
                }
            }
            // распознаватель не должен выгрузиться, пока мы слушаем
            if warm_at.elapsed() > Duration::from_secs(60) {
                warm_at = Instant::now();
                let _ = self.server_port();
            }
        }
        Ok(())
    }
}
