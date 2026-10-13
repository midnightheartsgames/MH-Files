//! Помощник автотегов: модель WD Tagger в ONNX Runtime — в отдельном процессе, как
//! `llama-server` у поиска по смыслу. Это тот же `MH-Files.exe`, запущенный с
//! [`HOST_FLAG`]: окна нет, говорит через stdin и stdout. Падение ONNX Runtime или драйвера
//! не роняет окно, а гигабайт модели уходит из памяти вместе с процессом.
//!
//! Протокол — кадры «вид (байт), длина (u32 LE), данные». Помощник первым шлёт `H` со
//! стороной картинки, числом тегов и устройством — `gpu` или `cpu` (у модели звука вместо
//! стороны — `sound`; или `E` с причиной и выходит); дальше на каждый `I` (числа f32 LE:
//! картинка NHWC или спектрограмма 1024×128) отвечает `O` (уверенность или логит каждого
//! тега) или `E`. Закрытый вход — конец работы.
//!
//! Модель звука (1.3.3) — в своём помощнике: тот же exe, другой `model.onnx`; какая это
//! модель, помощник понимает по форме входа.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout};

use mh_files_platform::process::Background;

/// Ключ командной строки помощника:
/// `MH-Files.exe --tagger-host <onnxruntime> <model.onnx> <потоки> <gpu|cpu>`.
pub const HOST_FLAG: &str = "--tagger-host";

const HELLO: u8 = b'H';
const INPUT: u8 = b'I';
const OUTPUT: u8 = b'O';
const FAIL: u8 = b'E';
/// Больше кадра не бывает: картинка 448×448 — 2,4 МБ.
const MAX_FRAME: usize = 64 << 20;

fn write_frame(out: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    out.write_all(&[kind])?;
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

fn read_frame(input: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    input.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "слишком большой кадр"));
    }
    let mut payload = vec![0u8; len];
    input.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

fn to_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn from_bytes(bytes: &[u8]) -> Option<Vec<f32>> {
    let (chunks, rest) = bytes.as_chunks::<4>();
    rest.is_empty().then(|| chunks.iter().map(|&c| f32::from_le_bytes(c)).collect())
}

/// Что модель берёт и отдаёт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    /// Сторона картинки на входе; 0 — модель звука (вход — спектрограмма
    /// [`mh_files_core::sound::FRAMES`] × [`mh_files_core::sound::MELS`]).
    pub side: usize,
    /// Оценок на выходе; 0 — модель не сказала заранее.
    pub outputs: usize,
    /// Модель работает на видеокарте (DirectML).
    pub gpu: bool,
}

impl Spec {
    pub fn sound(&self) -> bool {
        self.side == 0
    }

    /// Чисел на входе.
    pub fn input_len(&self) -> usize {
        if self.sound() {
            mh_files_core::sound::FRAMES * mh_files_core::sound::MELS
        } else {
            self.side * self.side * 3
        }
    }

    fn hello(&self) -> String {
        let device = if self.gpu { "gpu" } else { "cpu" };
        if self.sound() {
            format!("sound {} {device}", self.outputs)
        } else {
            format!("{} {} {device}", self.side, self.outputs)
        }
    }
}

/// Модель внутри помощника: вход — картинка, выход — уверенность каждого тега.
pub type Run<'a> = dyn FnMut(&[f32]) -> Result<Vec<f32>, String> + 'a;

/// Служить: `H` с формой модели (или `E` с причиной, почему она не загрузилась), затем на
/// каждый вход — ответ, пока вход не закроется.
pub fn serve(
    loaded: Result<Spec, String>,
    run: &mut Run,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<()> {
    let spec = match loaded {
        Ok(spec) => spec,
        Err(error) => return write_frame(output, FAIL, error.as_bytes()),
    };
    write_frame(output, HELLO, spec.hello().as_bytes())?;
    loop {
        let (kind, payload) = match read_frame(input) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let answer = match (kind, from_bytes(&payload)) {
            (INPUT, Some(values)) => run(&values),
            _ => Err("непонятный запрос".to_string()),
        };
        match answer {
            Ok(scores) => write_frame(output, OUTPUT, &to_bytes(&scores))?,
            Err(error) => write_frame(output, FAIL, error.as_bytes())?,
        }
    }
}

/// Точка входа помощника (аргументы после [`HOST_FLAG`]). Возвращает код выхода.
pub fn main(args: &[String]) -> i32 {
    let (runtime, model, threads, gpu) = match args {
        [runtime, model, threads] => (runtime, model, threads, false),
        [runtime, model, threads, device] => (runtime, model, threads, device == "gpu"),
        _ => {
            eprintln!("{HOST_FLAG} <onnxruntime> <model.onnx> <потоки> <gpu|cpu>");
            return 2;
        }
    };
    let threads = threads.parse().unwrap_or(1);
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    let result = match load(Path::new(runtime), Path::new(model), threads, gpu) {
        Ok((_environment, mut session, spec)) => {
            let mut run = |values: &[f32]| infer(&mut session, spec, values);
            serve(Ok(spec), &mut run, &mut input, &mut output)
        }
        Err(error) => {
            eprintln!("{error}");
            serve(Err(error), &mut |_| Err(String::new()), &mut input, &mut output)
        }
    };
    if result.is_ok() { 0 } else { 1 }
}

fn load(
    runtime: &Path,
    model: &Path,
    threads: usize,
    gpu: bool,
) -> Result<(ort::environment::Environment, ort::session::Session, Spec), String> {
    use mh_files_core::tags::{explain_runtime_error, input_side};
    use mh_files_platform::process::preload_library;
    // DirectML.dll — та, что лежит рядом: ONNX Runtime ищет её по имени, и Windows иначе
    // взяла бы старую из System32. Затем сам ONNX Runtime — его зависимости тоже из его папки.
    let directml = runtime.with_file_name("DirectML.dll");
    if gpu && directml.is_file() {
        let _ = preload_library(&directml);
    }
    preload_library(runtime).map_err(|e| explain_runtime_error(&e))?;
    let environment = ort::init_from(runtime)
        .map_err(|e| explain_runtime_error(&e.to_string()))?
        .with_name("MH Files")
        .build()
        .map_err(|e| explain_runtime_error(&e.to_string()))?;
    let fail = |e: ort::Error| format!("модель {} не загрузилась: {e}", model.display());
    let cpu = || -> Result<ort::session::Session, String> {
        ort::session::Session::builder(&environment)
            .map_err(|e| e.to_string())?
            .with_intra_threads(threads.max(1))
            .map_err(|e| e.to_string())?
            .with_inter_threads(1)
            .map_err(|e| e.to_string())?
            .commit_from_file(model)
            .map_err(fail)
    };
    let shape = |outlets: &[ort::value::Outlet]| -> Vec<i64> {
        outlets
            .first()
            .and_then(|o| o.dtype().tensor_shape())
            .map(|s| s.to_vec())
            .unwrap_or_default()
    };
    let spec_of = |session: &ort::session::Session, gpu: bool| -> Result<Spec, String> {
        let input = shape(session.inputs());
        let side = match input.as_slice() {
            // Спектрограмма AST: [N, 1024, 128].
            [_, frames, mels]
                if *frames == mh_files_core::sound::FRAMES as i64
                    && *mels == mh_files_core::sound::MELS as i64 =>
            {
                0
            }
            _ => input_side(&input)?,
        };
        let outputs =
            shape(session.outputs()).last().copied().filter(|&n| n > 0).unwrap_or(0) as usize;
        Ok(Spec { side, outputs, gpu })
    };
    if gpu {
        // Видеокарта: сессия с DirectML и пробный прогон пустой картинки. Не вышло —
        // процессор; причина — в журнале.
        let attempt = directml_session(&environment, model).and_then(|mut session| {
            let spec = spec_of(&session, true)?;
            infer(&mut session, spec, &vec![255.0; spec.input_len()])?;
            Ok((session, spec))
        });
        match attempt {
            Ok((session, spec)) => return Ok((environment, session, spec)),
            Err(error) => eprintln!("видеокарта не подошла, считается на процессоре: {error}"),
        }
    }
    let session = cpu()?;
    let spec = spec_of(&session, false)?;
    Ok((environment, session, spec))
}

/// Сессия на видеокарте. DirectML не умеет шаблоны памяти и параллельное выполнение, а
/// пакет из одной картинки он считает быстрее, если знает его заранее.
fn directml_session(
    environment: &ort::environment::Environment,
    model: &Path,
) -> Result<ort::session::Session, String> {
    let provider = ort::ep::DirectML::default().build().error_on_failure();
    ort::session::Session::builder(environment)
        .map_err(|e| e.to_string())?
        .with_memory_pattern(false)
        .map_err(|e| e.to_string())?
        .with_parallel_execution(false)
        .map_err(|e| e.to_string())?
        .with_dimension_override("batch_size", 1)
        .map_err(|e| e.to_string())?
        .with_execution_providers([provider])
        .map_err(|e| e.to_string())?
        .commit_from_file(model)
        .map_err(|e| e.to_string())
}

fn infer(
    session: &mut ort::session::Session,
    spec: Spec,
    values: &[f32],
) -> Result<Vec<f32>, String> {
    if values.len() != spec.input_len() {
        return Err(if spec.sound() {
            "спектрограмма не того размера".to_string()
        } else {
            format!("картинка не {0}×{0}", spec.side)
        });
    }
    let outputs = if spec.sound() {
        let shape = [1usize, mh_files_core::sound::FRAMES, mh_files_core::sound::MELS];
        let tensor =
            ort::value::TensorRef::from_array_view((shape, values)).map_err(|e| e.to_string())?;
        session.run(ort::inputs![tensor]).map_err(|e| e.to_string())?
    } else {
        let tensor =
            ort::value::TensorRef::from_array_view(([1usize, spec.side, spec.side, 3], values))
                .map_err(|e| e.to_string())?;
        session.run(ort::inputs![tensor]).map_err(|e| e.to_string())?
    };
    let (_, scores) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    Ok(scores.to_vec())
}

/// Как запустить помощника: программа и аргументы перед [`HOST_FLAG`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCommand {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

impl HostCommand {
    /// Этот же exe.
    pub fn current() -> Option<HostCommand> {
        Some(HostCommand { exe: std::env::current_exe().ok()?, args: Vec::new() })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// Помощник умер или говорит непонятное: перезапустить.
    Died(String),
    /// Модель отказалась от этой картинки: другие идут дальше.
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Died(error) | RunError::Failed(error) => f.write_str(error),
        }
    }
}

/// Разговор с помощником по каналам.
pub struct Channel<R: Read, W: Write> {
    input: R,
    output: W,
    pub spec: Spec,
}

impl<R: Read, W: Write> Channel<R, W> {
    /// Дождаться приветствия: модель загружена (секунды) или причина, почему нет.
    pub fn open(mut input: R, output: W) -> Result<Channel<R, W>, RunError> {
        match read_frame(&mut input) {
            Ok((HELLO, payload)) => {
                let text = String::from_utf8_lossy(&payload);
                let mut words = text.split_whitespace().peekable();
                let sound = words.peek() == Some(&"sound");
                if sound {
                    words.next();
                }
                let mut number = || words.next().and_then(|n| n.parse::<usize>().ok());
                let (side, outputs) =
                    if sound { (Some(0), number()) } else { (number(), number()) };
                let gpu = text.split_whitespace().last() == Some("gpu");
                match (side, outputs) {
                    (Some(side), Some(outputs)) if side > 0 || sound => {
                        Ok(Channel { input, output, spec: Spec { side, outputs, gpu } })
                    }
                    _ => Err(RunError::Died(format!("странное приветствие помощника: {text}"))),
                }
            }
            Ok((FAIL, payload)) => {
                Err(RunError::Failed(String::from_utf8_lossy(&payload).into_owned()))
            }
            Ok(_) => Err(RunError::Died("помощник ответил не по протоколу".into())),
            Err(error) => Err(RunError::Died(format!("помощник закрылся: {error}"))),
        }
    }

    pub fn run(&mut self, values: &[f32]) -> Result<Vec<f32>, RunError> {
        let died = |error: io::Error| RunError::Died(format!("помощник закрылся: {error}"));
        write_frame(&mut self.output, INPUT, &to_bytes(values)).map_err(died)?;
        match read_frame(&mut self.input).map_err(died)? {
            (OUTPUT, payload) => {
                from_bytes(&payload).ok_or_else(|| RunError::Died("ответ помощника обрезан".into()))
            }
            (FAIL, payload) => {
                Err(RunError::Failed(String::from_utf8_lossy(&payload).into_owned()))
            }
            _ => Err(RunError::Died("помощник ответил не по протоколу".into())),
        }
    }
}

/// Запущенный помощник. Уничтожение — конец процесса.
pub struct Host {
    channel: Channel<BufReader<ChildStdout>, BufWriter<ChildStdin>>,
    process: Background,
}

impl Host {
    /// Запустить и дождаться, пока модель загрузится. Вывод ONNX Runtime — в `log`.
    pub fn start(
        command: &HostCommand,
        runtime: &Path,
        model: &Path,
        threads: usize,
        gpu: bool,
        log: &Path,
    ) -> Result<Host, String> {
        if let Some(dir) = log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Журнал — только последнего запуска: он нужен, чтобы понять, почему не вышло.
        let _ = std::fs::write(
            log,
            format!("=== MH Files: запуск {} {}\n", runtime.display(), model.display()),
        );
        let mut args = command.args.clone();
        args.extend([
            HOST_FLAG.to_string(),
            runtime.display().to_string(),
            model.display().to_string(),
            threads.to_string(),
            if gpu { "gpu" } else { "cpu" }.to_string(),
        ]);
        let (process, stdin, stdout) = Background::spawn_piped(&command.exe, &args, Some(log))?;
        match Channel::open(BufReader::new(stdout), BufWriter::new(stdin)) {
            Ok(channel) => Ok(Host { channel, process }),
            Err(RunError::Failed(error)) => Err(error),
            Err(RunError::Died(error)) => {
                let tail = std::fs::read_to_string(log).unwrap_or_default();
                let reason =
                    tail.lines().rev().find(|l| !l.trim().is_empty() && !l.starts_with("==="));
                Err(match reason {
                    Some(line) => format!("{error}: {}", line.trim()),
                    None => error,
                })
            }
        }
    }

    pub fn spec(&self) -> Spec {
        self.channel.spec
    }

    pub fn run(&mut self, values: &[f32]) -> Result<Vec<f32>, RunError> {
        self.channel.run(values)
    }

    pub fn alive(&mut self) -> bool {
        self.process.exited().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Помощник в потоке, канал — трубы ОС: всё, как с процессом, только без процесса.
    fn connect(
        loaded: Result<Spec, String>,
    ) -> Result<Channel<io::PipeReader, io::PipeWriter>, RunError> {
        // Вход помощника: читает он, пишет клиент; выход — наоборот.
        let (host_reads, client_writes) = io::pipe().unwrap();
        let (client_reads, host_writes) = io::pipe().unwrap();
        std::thread::spawn(move || {
            let mut input = host_reads;
            let mut output = host_writes;
            // «Модель»: сумма входа и длина — два числа.
            let mut run = |values: &[f32]| {
                if values.first() == Some(&-1.0) {
                    return Err("битая картинка".to_string());
                }
                Ok(vec![values.iter().sum(), values.len() as f32])
            };
            let _ = serve(loaded, &mut run, &mut input, &mut output);
        });
        Channel::open(client_reads, client_writes)
    }

    #[test]
    fn talks_over_pipes() {
        let mut channel = connect(Ok(Spec { side: 2, outputs: 2, gpu: true })).unwrap();
        assert_eq!(channel.spec, Spec { side: 2, outputs: 2, gpu: true });
        assert_eq!(channel.run(&[1.0, 2.0, 3.5]).unwrap(), vec![6.5, 3.0]);
        assert_eq!(
            channel.run(&[-1.0]),
            Err(RunError::Failed("битая картинка".into())),
            "отказ на одну картинку — не смерть помощника"
        );
        assert_eq!(channel.run(&[0.5]).unwrap(), vec![0.5, 1.0]);
        let sound = connect(Ok(Spec { side: 0, outputs: 527, gpu: false })).unwrap();
        assert!(sound.spec.sound() && !sound.spec.gpu, "модель звука");
        assert_eq!(sound.spec.outputs, 527);
        assert_eq!(sound.spec.input_len(), 1024 * 128);
    }

    #[test]
    fn explains_why_the_model_did_not_load() {
        let error = connect(Err("нет MSVCP140.dll".into())).err().unwrap();
        assert_eq!(error, RunError::Failed("нет MSVCP140.dll".into()));
        // Помощник молча умер — тоже ошибка, а не зависание.
        let (reader, writer) = io::pipe().unwrap();
        drop(writer);
        let (_, sink) = io::pipe().unwrap();
        assert!(matches!(Channel::open(reader, sink), Err(RunError::Died(_))));
    }

    #[test]
    fn frames_are_checked() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, INPUT, &to_bytes(&[1.0, -2.0])).unwrap();
        let (kind, payload) = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!((kind, from_bytes(&payload)), (INPUT, Some(vec![1.0, -2.0])));
        assert_eq!(from_bytes(&[1, 2, 3]), None, "не кратно четырём");
        let huge = [INPUT, 0xFF, 0xFF, 0xFF, 0x7F];
        assert!(read_frame(&mut huge.as_slice()).is_err());
    }
}
