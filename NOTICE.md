# Чужой код в «Островке»

«Островок» распространяется по лицензии MIT (см. `LICENSE`). В нём использованы приёмы и фрагменты из другого открытого проекта:

## Atoll

Проект [Fuwni/dynamic-island-windows](https://github.com/Fuwni/dynamic-island-windows), лицензия MIT.
Copyright (c) 2026 mykolaichechelnytskyi

Оттуда перенесены на Rust: расчёт преломления стекла (`src/glass.rs`), подбор цвета по обложке (`src/media.rs`), прорисовка персонажей в пилюле и параметры пружин (`src/main.rs`). Места заимствований помечены в комментариях к коду.

Текст лицензии MIT, под которой распространяется Atoll:

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Библиотеки

Программа собрана с библиотеками Rust из crates.io (eframe/egui, windows, wry, ureq, serde, image, chrono и другими). Каждая распространяется по своей лицензии — MIT, Apache-2.0 или совместимой; полный список зависимостей — в `Cargo.lock`.
