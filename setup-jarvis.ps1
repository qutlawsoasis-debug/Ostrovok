# Скачивает всё, что нужно Джарвису, в %APPDATA%\Ostrovok\jarvis:
# движок языковой модели (llama.cpp), саму модель (Qwen3), распознавание русской речи (GigaAM) и голос (Piper).
# Запуск:  powershell -ExecutionPolicy Bypass -File setup-jarvis.ps1
# Размер модели:  -Model large (2,4 ГБ, по умолчанию) | medium (1 ГБ) | small (0,4 ГБ)
param([ValidateSet("large", "medium", "small")][string]$Model = "large")

$ErrorActionPreference = "Stop"
$dir = Join-Path $env:APPDATA "Ostrovok\jarvis"
New-Item -ItemType Directory -Force "$dir\engine", "$dir\models", "$dir\stt", "$dir\voice" | Out-Null

function Get-File($url, $out) {
    if (Test-Path $out) { Write-Host "уже есть: $out"; return }
    Write-Host "качаю: $url"
    curl.exe -L --fail --progress-bar -o "$out.part" $url
    if ($LASTEXITCODE -ne 0) { throw "не удалось скачать $url" }
    Move-Item -Force "$out.part" $out
}

# 1. движок языковой модели (работает и на видеокарте, и на одном процессоре)
if (-not (Test-Path "$dir\engine\llama-server.exe")) {
    Get-File "https://github.com/ggml-org/llama.cpp/releases/download/b12426/llama-b12426-bin-win-vulkan-x64.zip" "$dir\engine.zip"
    Expand-Archive -Force "$dir\engine.zip" "$dir\engine"
    Remove-Item "$dir\engine.zip"
}

# 2. языковая модель
$models = @{
    large  = "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf"
    medium = "https://huggingface.co/unsloth/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf"
    small  = "https://huggingface.co/unsloth/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf"
}
Get-File $models[$Model] "$dir\models\$Model.gguf"

# 3. распознавание русской речи
if (-not (Test-Path "$dir\stt\engine\bin\sherpa-onnx-offline-websocket-server.exe")) {
    Get-File "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-win-x64-shared-MD-MinSizeRel-no-tts.tar.bz2" "$dir\stt\engine.tar.bz2"
    tar.exe -xjf "$dir\stt\engine.tar.bz2" -C "$dir\stt"
    if ($LASTEXITCODE -ne 0) { throw "не удалось распаковать движок распознавания" }
    Rename-Item "$dir\stt\sherpa-onnx-v2.0.1-win-x64-shared-MD-MinSizeRel-no-tts" "engine"
    Remove-Item "$dir\stt\engine.tar.bz2"
}
if (-not (Test-Path "$dir\stt\model\model.int8.onnx")) {
    Get-File "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-ctc-punct-giga-am-v3-russian-2025-12-16.tar.bz2" "$dir\stt\model.tar.bz2"
    tar.exe -xjf "$dir\stt\model.tar.bz2" -C "$dir\stt"
    if ($LASTEXITCODE -ne 0) { throw "не удалось распаковать модель распознавания" }
    Rename-Item "$dir\stt\sherpa-onnx-nemo-ctc-punct-giga-am-v3-russian-2025-12-16" "model"
    Remove-Item "$dir\stt\model.tar.bz2"
}

# 4. голос
if (-not (Test-Path "$dir\voice\piper\piper.exe")) {
    Get-File "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip" "$dir\voice\piper.zip"
    Expand-Archive -Force "$dir\voice\piper.zip" "$dir\voice"
    Remove-Item "$dir\voice\piper.zip"
}
$voice = "https://huggingface.co/rhasspy/piper-voices/resolve/main/ru/ru_RU/irina/medium"
Get-File "$voice/ru_RU-irina-medium.onnx" "$dir\voice\voice.onnx"
Get-File "$voice/ru_RU-irina-medium.onnx.json" "$dir\voice\voice.onnx.json"

Write-Host ""
Write-Host "Готово. Перезапустите Островок и откройте раздел ИИ -> Джарвис."
