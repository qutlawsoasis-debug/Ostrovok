//! Голос Джарвиса: нейросетевая озвучка Piper, работает на процессоре без интернета.
//! Текст превращается в WAV отдельным процессом piper.exe, звук играет сама Windows.
//! Если в настройках есть ключ Fish Audio и номер голоса, фразу озвучивает их сервер (текст ответа уходит на api.fish.audio).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Duration;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT, SND_SYNC};

const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone)]
pub struct Voice {
    dir: PathBuf,
    /// номер последней фразы: устаревшая озвучка не начинает играть
    turn: Arc<AtomicU64>,
    /// сколько фраз сейчас звучит
    speaking: Arc<std::sync::atomic::AtomicUsize>,
}

/// Пока жив — считается, что Джарвис говорит.
struct Speaking(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Speaking {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Voice {
    pub fn new(dir: PathBuf) -> Self {
        Voice { dir, turn: Arc::new(AtomicU64::new(0)), speaking: Default::default() }
    }

    fn exe(&self) -> PathBuf {
        self.dir.join("voice").join("piper").join("piper.exe")
    }

    fn model(&self) -> PathBuf {
        self.dir.join("voice").join("voice.onnx")
    }

    /// Счётчик звучащих фраз — слух смотрит на него и молчит, пока он не ноль.
    pub fn speaking(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.speaking.clone()
    }

    pub fn ready(&self) -> bool {
        self.exe().exists() && self.model().exists()
    }

    fn play(&self, wav: &std::path::Path, turn: u64) {
        if self.turn.load(Ordering::Relaxed) != turn {
            return;
        }
        unsafe {
            let _ = PlaySoundW(&HSTRING::from(wav.as_os_str()), None, SND_FILENAME | SND_SYNC | SND_NODEFAULT);
        }
    }

    /// Замолчать сейчас.
    pub fn hush(&self) {
        self.turn.fetch_add(1, Ordering::Relaxed);
        unsafe {
            let _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
        }
    }

    /// Произнести текст; прошлую фразу обрывает.
    pub fn say(&self, text: &str, cloud: Option<(String, String)>) {
        let text = speakable(text);
        if text.is_empty() || (cloud.is_none() && !self.ready()) {
            return;
        }
        self.hush();
        let turn = self.turn.load(Ordering::Relaxed);
        let me = self.clone();
        self.speaking.fetch_add(1, Ordering::Relaxed);
        let guard = Speaking(self.speaking.clone());
        std::thread::spawn(move || {
            let _guard = guard;
            // два файла по очереди: прошлый может ещё быть занят проигрыванием
            let wav = std::env::temp_dir().join(format!("ostrovok-say-{}.wav", turn % 2));
            if let Some((key, voice)) = cloud {
                if me.fish_stream(&key, &voice, &text, turn) {
                    return;
                }
                // сервер не ответил — говорим встроенным голосом
                if !me.ready() {
                    return;
                }
            }
            let child = Command::new(me.exe())
                .arg("--model").arg(me.model())
                .arg("--output_file").arg(&wav)
                .args(["--sentence_silence", "0.15"])
                .current_dir(me.exe().parent().unwrap_or(&me.dir))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(NO_WINDOW)
                .spawn();
            let Ok(mut child) = child else { return };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
                let _ = stdin.write_all(b"\n");
            }
            if !child.wait().is_ok_and(|s| s.success()) {
                return;
            }
            me.play(&wav, turn);
        });
    }
}

/// Оставляет то, что можно произнести: без смайликов, разметки и ссылок.
fn speakable(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if word.starts_with("http://") || word.starts_with("https://") {
            out.push_str("ссылка ");
            continue;
        }
        let w: String = word.chars().filter(|c| c.is_alphanumeric() || ".,!?:;-–—%°+«»\"'()".contains(*c)).collect();
        if !w.is_empty() {
            out.push_str(&w);
            out.push(' ');
        }
    }
    out.trim().chars().take(700).collect()
}

impl Voice {
    /// Озвучка на сервере Fish Audio: звук начинает играть, как только пришло его начало.
    /// false — сервер не дал звука (тогда говорит встроенный голос).
    fn fish_stream(&self, key: &str, voice: &str, text: &str, turn: u64) -> bool {
        const SRC: f32 = 24000.0;
        let Some(device) = cpal::default_host().default_output_device() else { return false };
        let Ok(config) = device.default_output_config() else { return false };
        let rate = config.sample_rate() as u32 as f32;
        let ch = (config.channels() as usize).max(1);

        let body = serde_json::json!({ "text": text, "reference_id": voice, "format": "pcm", "sample_rate": 24000, "latency": "balanced" });
        let resp = ureq::post("https://api.fish.audio/v1/tts")
            .timeout(Duration::from_secs(40))
            .set("Authorization", &format!("Bearer {key}"))
            .set("model", "s2.1-pro-free")
            .send_json(body);
        let Ok(resp) = resp else { return false };
        let mut reader = resp.into_reader();

        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let go = Arc::new(AtomicBool::new(false));
        let err = |_e: cpal::Error| {};
        macro_rules! out {
            ($t:ty, $conv:expr) => {{
                let (queue, go) = (queue.clone(), go.clone());
                device.build_output_stream(
                    config.clone().into(),
                    move |data: &mut [$t], _: &_| {
                        let mut q = queue.lock().unwrap();
                        let live = go.load(Ordering::Relaxed);
                        for frame in data.chunks_mut(ch) {
                            let v = if live { q.pop_front().unwrap_or(0.0) } else { 0.0 };
                            for s in frame {
                                *s = $conv(v);
                            }
                        }
                    },
                    err,
                    None,
                )
            }};
        }
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => out!(f32, |v: f32| v),
            cpal::SampleFormat::I16 => out!(i16, |v: f32| (v * 32767.0) as i16),
            cpal::SampleFormat::U16 => out!(u16, |v: f32| ((v * 0.5 + 0.5) * 65535.0) as u16),
            cpal::SampleFormat::I32 => out!(i32, |v: f32| (v * 2147483647.0) as i32),
            _ => return false,
        };
        let Ok(stream) = stream else { return false };
        if stream.play().is_err() {
            return false;
        }

        // читаем звук кусками: 16 бит, моно; на лету приводим к частоте колонок
        let step = SRC / rate;
        let (mut pos, mut prev, mut got) = (0.0f32, 0.0f32, 0usize);
        let mut carry: Option<u8> = None;
        let mut buf = [0u8; 4096];
        let mut first = true;
        loop {
            if self.turn.load(Ordering::Relaxed) != turn {
                return true;
            }
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut bytes: Vec<u8> = Vec::with_capacity(n + 1);
            bytes.extend(carry.take());
            bytes.extend_from_slice(&buf[..n]);
            // на случай, если сервер всё же прислал заголовок WAV
            if first && bytes.len() >= 44 && &bytes[..4] == b"RIFF" {
                bytes.drain(..44);
            }
            first = false;
            if bytes.len() % 2 == 1 {
                carry = bytes.pop();
            }
            let mut q = queue.lock().unwrap();
            for pair in bytes.chunks_exact(2) {
                let cur = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0;
                while pos < 1.0 {
                    q.push_back(prev + (cur - prev) * pos);
                    pos += step;
                }
                pos -= 1.0;
                prev = cur;
                got += 1;
            }
            // небольшой запас, чтобы речь не заикалась
            if q.len() as f32 > rate * 0.2 {
                go.store(true, Ordering::Relaxed);
            }
        }
        if got == 0 {
            return false;
        }
        go.store(true, Ordering::Relaxed);
        while self.turn.load(Ordering::Relaxed) == turn && !queue.lock().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(30));
        }
        std::thread::sleep(Duration::from_millis(120));
        drop(stream);
        true
    }
}
