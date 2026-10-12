//! Что скачать и как говорить с моделью.
//!
//! * Модель — GGUF EmbeddingGemma 2 с Hugging Face (`unsloth/embeddinggemma-2-GGUF`):
//!   сам файл модели и `mmproj` — кодировщики картинок и звука. Имена файлов берутся из
//!   списка файлов репозитория, а не зашиты: так переживается переименование.
//! * Движок — `llama-server` из выпусков llama.cpp на GitHub (`llama-b<N>-bin-win-vulkan-x64.zip`:
//!   в нём и процессорные библиотеки, и Vulkan для видеокарты; нет драйвера Vulkan —
//!   llama.cpp молча считает на процессоре). EmbeddingGemma 2 поддерживается с b11452.
//! * Запросы — `POST /v1/embeddings`: текст строкой, картинка и звук — объектом
//!   `{"content":[часть]}` с `data:`-URI.

use serde_json::{Value, json};

use super::{DIMS, PROMPT_PROFILE};

/// Репозиторий модели по умолчанию.
pub const DEFAULT_REPO: &str = "unsloth/embeddinggemma-2-GGUF";
/// Квантование по умолчанию: Q8_0 почти не отличается от исходной модели и вдвое меньше.
pub const DEFAULT_QUANT: &str = "Q8_0";
/// Репозиторий llama.cpp на GitHub.
pub const RUNTIME_REPO: &str = "ggml-org/llama.cpp";
/// Первая сборка llama.cpp с EmbeddingGemma 2 (ggml-org/llama.cpp#30054).
pub const MIN_BUILD: u32 = 11452;
/// Картинка — ровно столько токенов: при другом числе вектор той же картинки другой.
pub const IMAGE_TOKENS: u32 = 280;

/// Путь API Hugging Face со списком файлов репозитория и их размерами.
pub fn repo_api_path(repo: &str) -> String {
    format!("/api/models/{repo}?blobs=true")
}

/// Путь для скачивания файла репозитория.
pub fn file_path(repo: &str, file: &str) -> String {
    format!("/{repo}/resolve/main/{file}")
}

/// Путь API GitHub со списком выпусков llama.cpp. Выпуски llama.cpp помечены
/// предварительными, поэтому `/releases/latest` их не видит — берётся список.
pub fn releases_api_path() -> String {
    format!("/repos/{RUNTIME_REPO}/releases?per_page=20")
}

/// Файл для скачивания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub name: String,
    /// Полный адрес `https://…`.
    pub url: String,
    /// Байт; 0 — неизвестно.
    pub size: u64,
}

/// Файлы модели в репозитории Hugging Face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFiles {
    pub model: Download,
    /// Кодировщики картинок и звука; без них ищется только по тексту.
    pub mmproj: Option<Download>,
}

/// Выбрать файлы модели по ответу `/api/models/<repo>?blobs=true`: модель с квантованием
/// `quant`, `mmproj` — того же квантования, иначе Q8_0, F16, BF16, F32 (кодировщики картинок
/// и звука весят больше самой модели: Q8_0 вдвое легче F16 почти без потерь).
pub fn choose_model_files(json: &str, repo: &str, quant: &str) -> Result<ModelFiles, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("непонятный ответ Hugging Face: {e}"))?;
    let siblings = value
        .get("siblings")
        .and_then(Value::as_array)
        .ok_or("в ответе Hugging Face нет списка файлов")?;
    let files: Vec<(String, u64)> = siblings
        .iter()
        .filter_map(|s| {
            let name = s.get("rfilename")?.as_str()?.to_string();
            let size = s.get("size").and_then(Value::as_u64).unwrap_or(0);
            name.to_lowercase().ends_with(".gguf").then_some((name, size))
        })
        .collect();
    let base = |name: &str| name.rsplit('/').next().unwrap_or(name).to_lowercase();
    let quant = quant.trim().to_lowercase();
    let download = |(name, size): &(String, u64)| Download {
        name: name.rsplit('/').next().unwrap_or(name).to_string(),
        url: format!("https://huggingface.co{}", file_path(repo, name)),
        size: *size,
    };
    let models: Vec<&(String, u64)> =
        files.iter().filter(|(name, _)| !base(name).starts_with("mmproj")).collect();
    let model = models
        .iter()
        .find(|(name, _)| tokens(&base(name)).contains(&quant))
        .or_else(|| models.iter().find(|(name, _)| base(name).contains(&quant)))
        .ok_or_else(|| {
            let found: Vec<String> = models.iter().map(|(n, _)| base(n)).collect();
            format!("в {repo} нет модели {quant}: есть {}", found.join(", "))
        })?;
    let projectors: Vec<&(String, u64)> =
        files.iter().filter(|(name, _)| base(name).starts_with("mmproj")).collect();
    let mmproj = [quant.as_str(), "q8_0", "f16", "bf16", "f32"]
        .iter()
        .find_map(|want| {
            projectors.iter().find(|(name, _)| tokens(&base(name)).contains(&want.to_string()))
        })
        .or_else(|| projectors.first())
        .map(|file| download(file));
    Ok(ModelFiles { model: download(model), mmproj })
}

/// Части имени файла: `embeddinggemma-2-q8_0.gguf` → `embeddinggemma`, `2`, `q8_0`, `gguf`.
fn tokens(name: &str) -> Vec<String> {
    name.split(['-', '.']).map(str::to_string).collect()
}

/// Выбрать сборку llama.cpp для Windows по ответу `/repos/…/releases`: новейшая, не старше
/// [`MIN_BUILD`], с Vulkan; нет такой — только для процессора.
pub fn choose_runtime(json: &str) -> Result<(String, Download), String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("непонятный ответ GitHub: {e}"))?;
    let releases = value.as_array().ok_or("в ответе GitHub нет списка выпусков")?;
    let mut best: Option<(u32, String, Download)> = None;
    for release in releases {
        let Some(tag) = release.get("tag_name").and_then(Value::as_str) else { continue };
        let Some(build) = tag.strip_prefix('b').and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if build < MIN_BUILD || best.as_ref().is_some_and(|(b, _, _)| *b >= build) {
            continue;
        }
        let assets = release.get("assets").and_then(Value::as_array).cloned().unwrap_or_default();
        let find = |suffix: &str| {
            assets.iter().find_map(|asset| {
                let name = asset.get("name")?.as_str()?;
                if name != format!("llama-{tag}-{suffix}") {
                    return None;
                }
                Some(Download {
                    name: name.to_string(),
                    url: asset.get("browser_download_url")?.as_str()?.to_string(),
                    size: asset.get("size").and_then(Value::as_u64).unwrap_or(0),
                })
            })
        };
        if let Some(download) =
            find("bin-win-vulkan-x64.zip").or_else(|| find("bin-win-cpu-x64.zip"))
        {
            best = Some((build, tag.to_string(), download));
        }
    }
    best.map(|(_, tag, download)| (tag, download)).ok_or_else(|| {
        format!("нет сборки llama.cpp для Windows новее b{MIN_BUILD} — проверьте позже")
    })
}

/// `https://host/path` → (`host`, `/path`).
pub fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let (host, path) = rest.split_at(slash);
    (!host.is_empty()).then_some((host, if path.is_empty() { "/" } else { path }))
}

/// Чем считались векторы. Другой файл модели или другие подсказки — индекс заново.
pub fn model_id(model_file: &str) -> String {
    format!("{model_file}|{PROMPT_PROFILE}|{DIMS}")
}

/// Как запустить `llama-server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub model: std::path::PathBuf,
    pub mmproj: Option<std::path::PathBuf>,
    pub port: u16,
    /// Считать на видеокарте, если она есть.
    pub gpu: bool,
    /// Потоков процессора; 0 — решает llama.cpp.
    pub threads: usize,
}

/// Аргументы командной строки `llama-server`.
pub fn server_args(config: &ServerConfig) -> Vec<String> {
    let mut args: Vec<String> = vec!["-m".into(), config.model.to_string_lossy().into_owned()];
    if let Some(mmproj) = &config.mmproj {
        args.extend(["--mmproj".into(), mmproj.to_string_lossy().into_owned()]);
        // Токенов на картинку — ровно IMAGE_TOKENS; пакет — с запасом на звук (30 с ≈ 750).
        let tokens = IMAGE_TOKENS.to_string();
        args.extend([
            "--image-min-tokens".into(),
            tokens.clone(),
            "--image-max-tokens".into(),
            tokens,
        ]);
    } else {
        args.push("--no-mmproj".into());
    }
    args.extend(
        [
            "--embeddings",
            "--pooling",
            "mean",
            // Вход для векторов целиком в одной порции: длинный кусок текста, картинка, звук.
            "-ub",
            "2048",
            "-b",
            "2048",
            "-c",
            "4096",
            "-np",
            "2",
            "--no-webui",
            "--host",
            "127.0.0.1",
            "--port",
        ]
        .map(String::from),
    );
    args.push(config.port.to_string());
    if !config.gpu {
        args.extend(["-ngl", "0", "--no-mmproj-offload", "-dev", "none"].map(String::from));
    }
    if config.threads > 0 {
        args.extend(["-t".into(), config.threads.to_string()]);
    }
    args
}

/// Что отдать модели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Уже с подсказкой ([`super::query_prompt`], [`super::document_prompt`]).
    Text(String),
    /// Картинка: тип (`image/jpeg`) и данные в base64.
    Image { mime: &'static str, base64: String },
    /// Звук (WAV, MP3) в base64: llama.cpp узнаёт формат по содержимому.
    Audio { base64: String },
}

/// Тело `POST /v1/embeddings`.
pub fn embeddings_request(inputs: &[Input]) -> String {
    let items: Vec<Value> = inputs
        .iter()
        .map(|input| match input {
            Input::Text(text) => Value::String(text.clone()),
            Input::Image { mime, base64 } => json!({"content": [{
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{base64}")}
            }]}),
            Input::Audio { base64 } => json!({"content": [{
                "type": "input_audio",
                "input_audio": {"data": base64}
            }]}),
        })
        .collect();
    json!({"input": items, "encoding_format": "float"}).to_string()
}

/// Векторы из ответа — в порядке входов. Каждый обрезан до [`DIMS`] и нормирован.
pub fn parse_embeddings(body: &str, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| format!("непонятный ответ модели: {e}"))?;
    if let Some(error) = value.get("error") {
        let message = error.get("message").and_then(Value::as_str).unwrap_or("ошибка");
        return Err(format!("модель: {message}"));
    }
    let data = value.get("data").and_then(Value::as_array).ok_or("в ответе модели нет векторов")?;
    let mut out: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (position, item) in data.iter().enumerate() {
        let index = item.get("index").and_then(Value::as_u64).map_or(position, |i| i as usize);
        let numbers = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or("в ответе модели вектор не числами")?;
        let raw: Vec<f32> = numbers.iter().map(|n| n.as_f64().unwrap_or(f64::NAN) as f32).collect();
        let vector = super::prepare(&raw)
            .ok_or_else(|| format!("модель вернула негодный вектор длины {}", raw.len()))?;
        let slot = out.get_mut(index).ok_or("в ответе модели лишний вектор")?;
        *slot = Some(vector);
    }
    out.into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| format!("модель вернула {} векторов из {expected}", data.len()))
}

/// Номер сборки llama.cpp из вывода `llama-server --version`: новые сборки пишут
/// «version: 1.2.3 (build 11539, commit 10a60cf)», старые — «version: 11539 (10a60cf)».
pub fn parse_build(text: &str) -> Option<u32> {
    let line = text.lines().find_map(|l| l.trim().strip_prefix("version:"))?;
    if let Some(rest) = line.split("(build ").nth(1) {
        return rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok();
    }
    line.split_whitespace().next()?.parse().ok()
}

/// Объяснение для людей, если llama.cpp не знает EmbeddingGemma 2.
pub fn too_old(build: Option<u32>) -> String {
    let which = build.map_or_else(String::new, |b| format!(" (сборка b{b})"));
    format!(
        "llama.cpp{which} слишком старый для EmbeddingGemma 2 — нужна сборка b{MIN_BUILD} или новее. Очистите поле «Свой llama-server», и MH Files скачает свежую, или обновите свой llama.cpp"
    )
}

/// Ошибка сервера о слишком длинном входе: такой вход повторять бесполезно.
pub fn too_large(message: &str) -> bool {
    message.contains("too large to process") || message.contains("larger than the max context")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HF: &str = r#"{"id":"unsloth/embeddinggemma-2-GGUF","siblings":[
        {"rfilename":".gitattributes","size":2000},
        {"rfilename":"README.md"},
        {"rfilename":"embeddinggemma-2-BF16.gguf","size":1500000000},
        {"rfilename":"embeddinggemma-2-Q4_K_M.gguf","size":300000000},
        {"rfilename":"embeddinggemma-2-Q8_0.gguf","size":800000000},
        {"rfilename":"mmproj-BF16.gguf","size":900000000},
        {"rfilename":"mmproj-F16.gguf","size":900000001},
        {"rfilename":"mmproj-F32.gguf","size":1800000000}]}"#;

    #[test]
    fn model_files_by_quant() {
        let files = choose_model_files(HF, DEFAULT_REPO, "Q8_0").unwrap();
        assert_eq!(files.model.name, "embeddinggemma-2-Q8_0.gguf");
        assert_eq!(
            files.model.url,
            "https://huggingface.co/unsloth/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf"
        );
        assert_eq!(files.model.size, 800_000_000);
        assert_eq!(files.mmproj.unwrap().name, "mmproj-F16.gguf");
        let small = choose_model_files(HF, DEFAULT_REPO, "q4_k_m").unwrap();
        assert_eq!(small.model.name, "embeddinggemma-2-Q4_K_M.gguf");
        let error = choose_model_files(HF, DEFAULT_REPO, "Q2_K").unwrap_err();
        assert!(
            error.contains("Q2_K".to_lowercase().as_str()) && error.contains("q8_0"),
            "{error}"
        );
        assert!(choose_model_files("{}", DEFAULT_REPO, "Q8_0").is_err());
        assert!(choose_model_files("не json", DEFAULT_REPO, "Q8_0").is_err());
    }

    /// Список файлов `unsloth/embeddinggemma-2-GGUF` как есть (октябрь 2026).
    #[test]
    fn model_files_of_the_real_repository() {
        let json = r#"{"siblings":[
            {"rfilename":".gitattributes","size":2265},{"rfilename":"README.md","size":22681},
            {"rfilename":"embeddinggemma-2-BF16.gguf","size":557950240},
            {"rfilename":"embeddinggemma-2-F16.gguf","size":557950240},
            {"rfilename":"embeddinggemma-2-Q8_0.gguf","size":309855520},
            {"rfilename":"embeddinggemma-2-UD-Q4_K_XL.gguf","size":175673856},
            {"rfilename":"embeddinggemma-2-UD-Q5_K_XL.gguf","size":210055680},
            {"rfilename":"embeddinggemma-2-UD-Q6_K_XL.gguf","size":248812032},
            {"rfilename":"mmproj-BF16.gguf","size":982074880},
            {"rfilename":"mmproj-F16.gguf","size":980895232},
            {"rfilename":"mmproj-Q8_0.gguf","size":554821120}]}"#;
        let pick = |quant| {
            let files = choose_model_files(json, DEFAULT_REPO, quant).unwrap();
            (files.model.name, files.mmproj.unwrap().name)
        };
        assert_eq!(pick("Q8_0"), ("embeddinggemma-2-Q8_0.gguf".into(), "mmproj-Q8_0.gguf".into()));
        assert_eq!(
            pick("UD-Q4_K_XL"),
            ("embeddinggemma-2-UD-Q4_K_XL.gguf".into(), "mmproj-Q8_0.gguf".into())
        );
        assert_eq!(pick("BF16"), ("embeddinggemma-2-BF16.gguf".into(), "mmproj-BF16.gguf".into()));
        assert_eq!(pick("F16"), ("embeddinggemma-2-F16.gguf".into(), "mmproj-F16.gguf".into()));
    }

    #[test]
    fn model_files_of_other_naming() {
        // Как у ggml-org: mmproj с именем модели, файлы в папке.
        let json = r#"{"siblings":[
            {"rfilename":"gguf/embeddinggemma-2-Q8_0.gguf"},
            {"rfilename":"gguf/mmproj-embeddinggemma-2-Q8_0.gguf"}]}"#;
        let files = choose_model_files(json, "ggml-org/embeddinggemma-2-GGUF", "Q8_0").unwrap();
        assert_eq!(files.model.name, "embeddinggemma-2-Q8_0.gguf");
        assert!(files.model.url.ends_with("/resolve/main/gguf/embeddinggemma-2-Q8_0.gguf"));
        assert_eq!(files.mmproj.unwrap().name, "mmproj-embeddinggemma-2-Q8_0.gguf");
        let text_only = r#"{"siblings":[{"rfilename":"embeddinggemma-2-Q8_0.gguf"}]}"#;
        assert!(choose_model_files(text_only, "x/y", "Q8_0").unwrap().mmproj.is_none());
    }

    #[test]
    fn runtime_is_newest_supported_vulkan_build() {
        let asset = |tag: &str, kind: &str| {
            format!(
                r#"{{"name":"llama-{tag}-bin-win-{kind}-x64.zip","size":42,"browser_download_url":"https://github.com/ggml-org/llama.cpp/releases/download/{tag}/llama-{tag}-bin-win-{kind}-x64.zip"}}"#
            )
        };
        let json = format!(
            r#"[{{"tag_name":"b11520","prerelease":true,"assets":[{}, {}, {{"name":"cudart-llama-bin-win-cuda-12.4-x64.zip"}}]}},
                {{"tag_name":"b11519","assets":[]}},
                {{"tag_name":"b11530","assets":[{}]}},
                {{"tag_name":"b11400","assets":[{}]}},
                {{"tag_name":"v1.2.3","assets":[]}}]"#,
            asset("b11520", "cpu"),
            asset("b11520", "vulkan"),
            asset("b11530", "cpu"),
            asset("b11400", "vulkan"),
        );
        let (tag, download) = choose_runtime(&json).unwrap();
        assert_eq!(tag, "b11530", "новейшая, хоть и только для процессора");
        assert_eq!(download.name, "llama-b11530-bin-win-cpu-x64.zip");
        let json = json.replace("b11530", "b11510-old");
        let (tag, download) = choose_runtime(&json).unwrap();
        assert_eq!(tag, "b11520");
        assert_eq!(download.name, "llama-b11520-bin-win-vulkan-x64.zip");
        assert_eq!(download.size, 42);
        assert!(choose_runtime(r#"[{"tag_name":"b11400","assets":[]}]"#).is_err());
        assert!(choose_runtime("{}").is_err());
    }

    #[test]
    fn urls_split() {
        assert_eq!(split_url("https://huggingface.co/a/b"), Some(("huggingface.co", "/a/b")));
        assert_eq!(split_url("https://example.com"), Some(("example.com", "/")));
        assert_eq!(split_url("http://example.com/x"), None);
        assert_eq!(split_url("https:///x"), None);
    }

    #[test]
    fn server_arguments() {
        let mut config = ServerConfig {
            model: "C:/m/model.gguf".into(),
            mmproj: Some("C:/m/mmproj.gguf".into()),
            port: 41234,
            gpu: true,
            threads: 0,
        };
        let args = server_args(&config).join(" ");
        assert!(args.starts_with("-m C:/m/model.gguf --mmproj C:/m/mmproj.gguf"), "{args}");
        assert!(args.contains("--image-min-tokens 280 --image-max-tokens 280"));
        assert!(args.contains("--embeddings --pooling mean -ub 2048 -b 2048"));
        assert!(args.contains("--host 127.0.0.1 --port 41234"));
        assert!(!args.contains("-ngl") && !args.contains(" -t "));
        config.gpu = false;
        config.threads = 3;
        config.mmproj = None;
        let args = server_args(&config).join(" ");
        assert!(args.contains("--no-mmproj --embeddings"));
        assert!(args.contains("-ngl 0 --no-mmproj-offload -dev none -t 3"), "{args}");
    }

    #[test]
    fn request_body_shapes() {
        let body = embeddings_request(&[
            Input::Text("task: search result | query: кот".into()),
            Input::Image { mime: "image/jpeg", base64: "QUJD".into() },
            Input::Audio { base64: "UklG".into() },
        ]);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["input"][0], "task: search result | query: кот");
        assert_eq!(value["input"][1]["content"][0]["type"], "image_url");
        assert_eq!(
            value["input"][1]["content"][0]["image_url"]["url"],
            "data:image/jpeg;base64,QUJD"
        );
        assert_eq!(value["input"][2]["content"][0]["input_audio"]["data"], "UklG");
        assert_eq!(value["encoding_format"], "float");
    }

    #[test]
    fn responses_in_input_order() {
        let vector = |first: f32| {
            let mut v = vec![0.0f32; 768];
            v[0] = first;
            v[1] = 1.0;
            serde_json::to_string(&v).unwrap()
        };
        let body = format!(
            r#"{{"object":"list","data":[{{"index":1,"embedding":{}}},{{"index":0,"embedding":{}}}],"usage":{{"prompt_tokens":3}}}}"#,
            vector(0.0),
            vector(1.0)
        );
        let vectors = parse_embeddings(&body, 2).unwrap();
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].len(), DIMS);
        assert!((vectors[0][0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(vectors[1][0], 0.0);
        assert!(parse_embeddings(&body, 3).is_err(), "не хватает векторов");
        let error = r#"{"error":{"code":500,"message":"input (3000 tokens) is too large to process. increase the physical batch size","type":"server_error"}}"#;
        let message = parse_embeddings(error, 1).unwrap_err();
        assert!(too_large(&message), "{message}");
        let short = r#"{"data":[{"index":0,"embedding":[1.0,2.0]}]}"#;
        assert!(parse_embeddings(short, 1).is_err(), "короче DIMS");
        assert!(parse_embeddings("<html>", 1).is_err());
    }

    #[test]
    fn build_numbers_of_both_formats() {
        let new = "version: 1.2.3 (build 11539, commit 10a60cf30)\nbuilt with MSVC for x64";
        assert_eq!(parse_build(new), Some(11539));
        let old =
            "ggml_vulkan: Found 1 Vulkan devices\nversion: 6123 (b92761a51)\nbuilt with clang";
        assert_eq!(parse_build(old), Some(6123));
        assert_eq!(parse_build("нет версии"), None);
        assert!(too_old(Some(6123)).contains("b6123") && too_old(None).contains("b11452"));
    }

    #[test]
    fn model_id_changes_with_file() {
        assert_ne!(model_id("a-Q8_0.gguf"), model_id("a-Q4_K_M.gguf"));
        assert!(model_id("x.gguf").contains(PROMPT_PROFILE));
    }
}
