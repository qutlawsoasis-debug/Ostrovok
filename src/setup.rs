//! Установка Джарвиса из самого острова: скачивает движки и модели в папку настроек и распаковывает их.
//! Ничего из скачанного не запускает — только кладёт файлы.

use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NO_WINDOW: u32 = 0x0800_0000;

/// Что делать со скачанным файлом.
enum Kind {
    /// просто файл
    File,
    /// архив: распаковать в папку; если задано — переименовать появившуюся папку
    Archive { into: &'static str, rename: Option<(&'static str, &'static str)> },
}

struct Step {
    /// как называется для человека
    label: &'static str,
    url: &'static str,
    /// куда скачать, относительно папки Джарвиса
    file: &'static str,
    kind: Kind,
    /// если этот путь уже есть — шаг не нужен
    marker: &'static str,
    /// примерный размер, МБ — для подписи до начала загрузки
    mb: u32,
}

const BASE: [Step; 6] = [
    Step {
        label: "Движок модели",
        url: "https://github.com/ggml-org/llama.cpp/releases/download/b12426/llama-b12426-bin-win-vulkan-x64.zip",
        file: "engine.zip",
        kind: Kind::Archive { into: "engine", rename: None },
        marker: "engine/llama-server.exe",
        mb: 36,
    },
    Step {
        label: "Движок распознавания речи",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-win-x64-shared-MD-MinSizeRel-no-tts.tar.bz2",
        file: "stt/engine.tar.bz2",
        kind: Kind::Archive { into: "stt", rename: Some(("sherpa-onnx-v2.0.1-win-x64-shared-MD-MinSizeRel-no-tts", "engine")) },
        marker: "stt/engine/bin/sherpa-onnx-offline-websocket-server.exe",
        mb: 17,
    },
    Step {
        label: "Модель распознавания речи",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-ctc-punct-giga-am-v3-russian-2025-12-16.tar.bz2",
        file: "stt/model.tar.bz2",
        kind: Kind::Archive { into: "stt", rename: Some(("sherpa-onnx-nemo-ctc-punct-giga-am-v3-russian-2025-12-16", "model")) },
        marker: "stt/model/model.int8.onnx",
        mb: 155,
    },
    Step {
        label: "Движок голоса",
        url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip",
        file: "voice/piper.zip",
        kind: Kind::Archive { into: "voice", rename: None },
        marker: "voice/piper/piper.exe",
        mb: 21,
    },
    Step {
        label: "Голос",
        url: "https://huggingface.co/rhasspy/piper-voices/resolve/main/ru/ru_RU/irina/medium/ru_RU-irina-medium.onnx",
        file: "voice/voice.onnx",
        kind: Kind::File,
        marker: "voice/voice.onnx",
        mb: 60,
    },
    Step {
        label: "Описание голоса",
        url: "https://huggingface.co/rhasspy/piper-voices/resolve/main/ru/ru_RU/irina/medium/ru_RU-irina-medium.onnx.json",
        file: "voice/voice.onnx.json",
        kind: Kind::File,
        marker: "voice/voice.onnx.json",
        mb: 0,
    },
];

fn model_step(model: &str) -> Step {
    match model {
        "small" => Step {
            label: "Малая модель",
            url: "https://huggingface.co/unsloth/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf",
            file: "models/small.gguf",
            kind: Kind::File,
            marker: "models/small.gguf",
            mb: 378,
        },
        "medium" => Step {
            label: "Средняя модель",
            url: "https://huggingface.co/unsloth/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf",
            file: "models/medium.gguf",
            kind: Kind::File,
            marker: "models/medium.gguf",
            mb: 1056,
        },
        _ => Step {
            label: "Большая модель",
            url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf",
            file: "models/large.gguf",
            kind: Kind::File,
            marker: "models/large.gguf",
            mb: 2381,
        },
    }
}

#[derive(Default, Clone)]
pub struct Progress {
    pub running: bool,
    /// что качается сейчас
    pub label: String,
    /// номер шага и сколько их всего
    pub step: (usize, usize),
    /// байты текущего файла: скачано и всего (0 — размер неизвестен)
    pub bytes: (u64, u64),
    pub error: Option<String>,
    /// установка дошла до конца
    pub finished: bool,
}

#[derive(Clone)]
pub struct Setup {
    dir: PathBuf,
    state: Arc<Mutex<Progress>>,
}

impl Setup {
    pub fn new(dir: PathBuf) -> Self {
        Setup { dir, state: Default::default() }
    }

    pub fn progress(&self) -> Progress {
        self.state.lock().unwrap().clone()
    }

    /// Всё ли на месте, чтобы Джарвис хотя бы думал: движок и какая-нибудь модель.
    pub fn brain_ready(&self) -> bool {
        self.dir.join("engine/llama-server.exe").exists() && ["large", "medium", "small"].iter().any(|m| self.dir.join(format!("models/{m}.gguf")).exists())
    }

    /// Сколько мегабайт осталось скачать для выбранной модели.
    pub fn need_mb(&self, model: &str) -> u32 {
        self.steps(model).iter().map(|s| s.mb).sum()
    }

    fn steps(&self, model: &str) -> Vec<Step> {
        BASE.into_iter().chain([model_step(model)]).filter(|s| !self.dir.join(s.marker).exists()).collect()
    }

    /// Качает недостающее в отдельном потоке; repaint дёргается при каждом заметном изменении.
    pub fn start(&self, model: &str, repaint: impl Fn() + Send + 'static) {
        {
            let mut st = self.state.lock().unwrap();
            if st.running {
                return;
            }
            *st = Progress { running: true, ..Default::default() };
        }
        let (me, model) = (self.clone(), model.to_string());
        std::thread::spawn(move || {
            let res = me.run(&model, &repaint);
            let mut st = me.state.lock().unwrap();
            st.running = false;
            match res {
                Ok(()) => st.finished = true,
                Err(e) => st.error = Some(e),
            }
            drop(st);
            repaint();
        });
    }

    fn run(&self, model: &str, repaint: &dyn Fn()) -> Result<(), String> {
        let steps = self.steps(model);
        let total = steps.len();
        for (i, step) in steps.iter().enumerate() {
            {
                let mut st = self.state.lock().unwrap();
                st.label = step.label.to_string();
                st.step = (i + 1, total);
                st.bytes = (0, step.mb as u64 * 1_048_576);
            }
            repaint();
            let file = self.dir.join(step.file);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("Не удалось создать папку: {e}"))?;
            }
            download(step.url, &file, |done, all| {
                let mut st = self.state.lock().unwrap();
                st.bytes = (done, all.max(st.bytes.1.min(done)));
                if all > 0 {
                    st.bytes.1 = all;
                }
                drop(st);
                repaint();
            })
            .map_err(|e| format!("{}: {e}", step.label))?;
            if let Kind::Archive { into, rename } = &step.kind {
                self.state.lock().unwrap().label = format!("{} — распаковка", step.label);
                repaint();
                let into = self.dir.join(into);
                std::fs::create_dir_all(&into).map_err(|e| e.to_string())?;
                unpack(&file, &into).map_err(|e| format!("{}: {e}", step.label))?;
                if let Some((from, to)) = rename {
                    let _ = std::fs::remove_dir_all(into.join(to));
                    std::fs::rename(into.join(from), into.join(to)).map_err(|e| format!("{}: не удалось переименовать папку: {e}", step.label))?;
                }
                let _ = std::fs::remove_file(&file);
            }
            if !self.dir.join(step.marker).exists() {
                return Err(format!("{}: после установки не найден файл {}", step.label, step.marker));
            }
        }
        Ok(())
    }
}

/// Скачивает файл целиком во временный «.part» и только потом переименовывает:
/// оборванная загрузка не выглядит готовым файлом.
fn download(url: &str, to: &Path, mut on: impl FnMut(u64, u64)) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(60)).build();
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(c, _) => format!("сервер ответил {c}"),
        _ => "нет связи с сервером".to_string(),
    })?;
    let total: u64 = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let part = to.with_extension(format!("{}.part", to.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let mut out = std::fs::File::create(&part).map_err(|e| format!("не удалось создать файл: {e}"))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let (mut done, mut shown) = (0u64, 0u64);
    loop {
        let n = reader.read(&mut buf).map_err(|_| "загрузка оборвалась".to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("не удалось записать файл (кончилось место?): {e}"))?;
        done += n as u64;
        // не чаще, чем раз в мегабайт
        if done - shown > 1_048_576 {
            shown = done;
            on(done, total);
        }
    }
    drop(out);
    if total > 0 && done != total {
        let _ = std::fs::remove_file(&part);
        return Err("загрузка оборвалась".into());
    }
    on(done, total);
    std::fs::rename(&part, to).map_err(|e| format!("не удалось сохранить файл: {e}"))
}

/// Распаковывает архив: tar.bz2 — своими силами, zip — системным tar.exe.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    // системный tar.exe архивы .bz2 сам не умеет (ищет постороннюю программу bzip2 и зависает),
    // поэтому их распаковываем сами
    if archive.extension().is_some_and(|e| e == "bz2") {
        let file = std::fs::File::open(archive).map_err(|e| format!("не удалось открыть архив: {e}"))?;
        let plain = bzip2_rs::DecoderReader::new(std::io::BufReader::with_capacity(1 << 20, file));
        return tar::Archive::new(plain).unpack(into).map_err(|e| format!("архив не распаковался: {e}"));
    }
    let tar = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("System32").join("tar.exe");
    let out = Command::new(if tar.exists() { tar } else { "tar.exe".into() })
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW)
        .output()
        .map_err(|e| format!("не удалось запустить распаковку: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("архив не распаковался: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(160).collect::<String>()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Настоящая загрузка из сети: движок голоса (zip) и движок распознавания (tar.bz2).
    /// Запуск: cargo test --release setup -- --ignored
    #[test]
    #[ignore]
    fn downloads_and_unpacks() {
        let dir = std::env::temp_dir().join("ostrovok-setup-test");
        let _ = std::fs::remove_dir_all(&dir);
        for i in [3usize, 1] {
            let step = &BASE[i];
            let file = dir.join(step.file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            let mut last = (0, 0);
            download(step.url, &file, |d, t| last = (d, t)).unwrap();
            assert!(last.0 > 1_000_000 && (last.1 == 0 || last.0 == last.1), "{last:?}");
            let Kind::Archive { into, rename } = &step.kind else { panic!() };
            unpack(&file, &dir.join(into)).unwrap();
            if let Some((from, to)) = rename {
                std::fs::rename(dir.join(into).join(from), dir.join(into).join(to)).unwrap();
            }
            assert!(dir.join(step.marker).exists(), "нет {}", step.marker);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
