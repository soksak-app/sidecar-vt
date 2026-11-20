//! Measures the output of full-screen programs after a scroll, for the quiet interval of the program scroll of core
//! `docs/spec/terminal-read.md#program-scroll` (core F899-8.6, S211). For each program of `--programs` (`less`, `vim` and `htop`) it
//! runs the program in a real PTY of `--rows` by `--cols` cells with `TERM=xterm-256color`, moves it to the end of a numbered file of
//! 2000 lines (`less` and `vim`), and then `--repeats` times sends `--by` cursor keys at once, as the program scroll sends them, upward
//! and downward in turn (`htop` starts at its first process, so its first scroll goes downward). The cursor keys follow the cursor key mode that the program set (`CSI ? 1 h` gives `ESC O A`). Each read of
//! the PTY is one output; after each scroll it records the time from the keys to the first output and the gaps between the outputs,
//! until no output came for `--observe-ms`, which bounds the observation and is no judgment. With `--load` it runs one `yes` process
//! for each processor of the system during the repeats and ends them before it reports.
//!
//! It prints, per program and load, the largest time to the first output, the largest gap between two outputs of one redraw and the
//! largest of both in display frames at each refresh rate of `--refresh-rates`, which the make target reads from the screens of the
//! system: the frames of a time at a rate are the time multiplied by the rate, rounded up. A gap is measured only below the observation
//! bound, so the bound is chosen well above the gaps that it reports. A scroll that gave no output and a program that does not start are
//! errors, and the exit status is 1. It judges no value and gates
//! nothing. `make measure-program-redraw` runs it.
//!
//!   cargo run --release -p soksak-sidecar-vt-core --example measure_program_redraw -- --refresh-rates 120[,60]
//!     [--programs less,vim,htop] [--repeats 20] [--by 50] [--rows 40] [--cols 120] [--observe-ms 2000] [--load]

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// The options of a run.
struct Options {
    programs: Vec<String>,
    repeats: usize,
    by: usize,
    rows: u16,
    cols: u16,
    observe: Duration,
    load: bool,
    refresh_rates: Vec<u32>,
}

/// The outputs that followed one scroll: the time from the keys to the first output and the gaps between outputs.
struct Redraw {
    first: Duration,
    gaps: Vec<Duration>,
    outputs: usize,
}

fn main() {
    let options = match parse(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: measure-program-redraw: {error}");
            std::process::exit(2);
        }
    };
    let mut failed = false;
    for program in &options.programs {
        match measure(program, &options) {
            Ok(redraws) => report(program, &options, &redraws),
            Err(error) => {
                eprintln!("error: measure-program-redraw: {program}: {error}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn parse(arguments: Vec<String>) -> Result<Options, String> {
    let mut options = Options {
        programs: vec!["less".into(), "vim".into(), "htop".into()],
        repeats: 20,
        by: 50,
        rows: 40,
        cols: 120,
        observe: Duration::from_millis(2000),
        load: false,
        refresh_rates: Vec::new(),
    };
    let mut arguments = arguments.into_iter();
    while let Some(name) = arguments.next() {
        if name == "--load" {
            options.load = true;
            continue;
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("{name} needs a value"))?;
        let number = |value: &str| {
            value
                .parse::<u64>()
                .map_err(|_| format!("{name} is not a positive integer: {value:?}"))
        };
        match name.as_str() {
            "--programs" => options.programs = value.split(',').map(str::to_string).collect(),
            "--repeats" => options.repeats = number(&value)? as usize,
            "--by" => options.by = number(&value)? as usize,
            "--rows" => options.rows = number(&value)? as u16,
            "--cols" => options.cols = number(&value)? as u16,
            "--observe-ms" => options.observe = Duration::from_millis(number(&value)?),
            "--refresh-rates" => {
                options.refresh_rates = value
                    .split(',')
                    .map(|rate| number(rate).map(|rate| rate as u32))
                    .collect::<Result<_, _>>()?
            }
            _ => return Err(format!("unknown argument {name}")),
        }
    }
    if options.refresh_rates.is_empty() || options.refresh_rates.contains(&0) {
        return Err("--refresh-rates needs the refresh rates of the screens, each above 0".into());
    }
    if options.repeats == 0 || options.by == 0 || options.rows == 0 || options.cols == 0 {
        return Err("--repeats, --by, --rows and --cols must be above 0".into());
    }
    for program in &options.programs {
        if !["less", "vim", "htop"].contains(&program.as_str()) {
            return Err(format!(
                "--programs names {program:?}; the programs are less, vim and htop"
            ));
        }
    }
    Ok(options)
}

/// Runs one program in a PTY and returns the redraws of its scrolls.
fn measure(program: &str, options: &Options) -> Result<Vec<Redraw>, String> {
    let folder = tempfile::tempdir().map_err(|error| format!("temporary folder: {error}"))?;
    let file = folder.path().join("numbered.txt");
    let text: String = (1..=2000).map(|line| format!("line {line:04}\n")).collect();
    std::fs::write(&file, text).map_err(|error| format!("{}: {error}", file.display()))?;
    let mut command = match program {
        "less" => {
            let mut command = CommandBuilder::new("less");
            command.arg(&file);
            command
        }
        "vim" => {
            let mut command = CommandBuilder::new("vim");
            for argument in [
                "-u",
                "NONE",
                "-N",
                "-n",
                "-i",
                "NONE",
                "--cmd",
                "set t_RV= t_u7=",
            ] {
                command.arg(argument);
            }
            command.arg(&file);
            command
        }
        _ => {
            let mut command = CommandBuilder::new("htop");
            // A delay of 100 tenths of a second keeps the periodic redraw of htop out of the observation of a scroll.
            command.args(["-d", "100"]);
            command
        }
    };
    command.env("TERM", "xterm-256color");
    command.cwd(folder.path());
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: options.rows,
            cols: options.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("openpty: {error}"))?;
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("spawn: {error}"))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("reader: {error}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("writer: {error}"))?;
    let (sender, outputs) = channel::<(Instant, Vec<u8>)>();
    let reading = std::thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(count) => {
                    if sender
                        .send((Instant::now(), buffer[..count].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });

    let mut cursor_keys = CursorKeys::default();
    let result = (|| {
        let start = settle(&outputs, options.observe, &mut cursor_keys)?;
        if start.outputs == 0 {
            return Err("the program wrote no screen".to_string());
        }
        if program != "htop" {
            send(&mut writer, b"G")?;
            settle(&outputs, options.observe, &mut cursor_keys)?;
        }
        let load = if options.load {
            Load::start()?
        } else {
            Load::default()
        };
        let mut redraws = Vec::new();
        for repeat in 0..options.repeats {
            // less and vim start each run at the end of the file and htop at its first process, so the first scroll of less and vim
            // goes up and that of htop down.
            let up = (repeat % 2 == 0) != (program == "htop");
            let key = if up {
                cursor_keys.up()
            } else {
                cursor_keys.down()
            };
            let keys = key.repeat(options.by);
            send(&mut writer, &keys)?;
            let sent = Instant::now();
            let redraw = settle_after(&outputs, sent, options.observe, &mut cursor_keys)?;
            if redraw.outputs == 0 {
                return Err(format!(
                    "scroll {} of {} gave no output within {:?}",
                    repeat + 1,
                    options.repeats,
                    options.observe
                ));
            }
            redraws.push(redraw);
        }
        load.end()?;
        Ok(redraws)
    })();
    let quit: &[u8] = match program {
        "vim" => b"\x1b:qa!\r",
        _ => b"q",
    };
    let quit_sent = send(&mut writer, quit);
    drop(writer);
    let ended = child.wait().map_err(|error| format!("wait: {error}"));
    drop(pair.master);
    reading
        .join()
        .map_err(|_| "the reading thread panicked".to_string())?;
    let redraws = result?;
    quit_sent?;
    let status = ended?;
    if !status.success() {
        return Err(format!("the program ended with {status:?}"));
    }
    Ok(redraws)
}

fn send(writer: &mut Box<dyn Write + Send>, bytes: &[u8]) -> Result<(), String> {
    writer
        .write_all(bytes)
        .and_then(|()| writer.flush())
        .map_err(|error| format!("write: {error}"))
}

/// Reads outputs until none came for `observe`, and returns them as one redraw from the first output.
fn settle(
    outputs: &Receiver<(Instant, Vec<u8>)>,
    observe: Duration,
    keys: &mut CursorKeys,
) -> Result<Redraw, String> {
    settle_after(outputs, Instant::now(), observe, keys)
}

/// Reads outputs after `sent` until none came for `observe`, and returns the time to the first output and the gaps between them.
fn settle_after(
    outputs: &Receiver<(Instant, Vec<u8>)>,
    sent: Instant,
    observe: Duration,
    keys: &mut CursorKeys,
) -> Result<Redraw, String> {
    let mut times = Vec::new();
    loop {
        match outputs.recv_timeout(observe) {
            Ok((at, bytes)) => {
                keys.scan(&bytes);
                times.push(at);
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => return Err("the PTY closed".into()),
        }
    }
    let first = times
        .first()
        .map_or(Duration::ZERO, |at| at.saturating_duration_since(sent));
    let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    Ok(Redraw {
        first,
        gaps,
        outputs: times.len(),
    })
}

/// The cursor key mode (DECCKM) that the output of the program set.
#[derive(Default)]
struct CursorKeys {
    application: bool,
}

impl CursorKeys {
    fn scan(&mut self, bytes: &[u8]) {
        for window in bytes.windows(5) {
            match window {
                b"\x1b[?1h" => self.application = true,
                b"\x1b[?1l" => self.application = false,
                _ => {}
            }
        }
    }

    fn up(&self) -> Vec<u8> {
        if self.application {
            b"\x1bOA".to_vec()
        } else {
            b"\x1b[A".to_vec()
        }
    }

    fn down(&self) -> Vec<u8> {
        if self.application {
            b"\x1bOB".to_vec()
        } else {
            b"\x1b[B".to_vec()
        }
    }
}

/// The `yes` processes that load each processor during the repeats.
#[derive(Default)]
struct Load {
    children: Vec<Child>,
}

impl Load {
    fn start() -> Result<Load, String> {
        let count = std::thread::available_parallelism()
            .map_err(|error| format!("processor count: {error}"))?
            .get();
        let mut load = Load::default();
        for _ in 0..count {
            let child = Command::new("yes")
                .stdout(Stdio::null())
                .spawn()
                .map_err(|error| format!("start yes: {error}"))?;
            load.children.push(child);
        }
        Ok(load)
    }

    fn end(mut self) -> Result<(), String> {
        for child in &mut self.children {
            child
                .kill()
                .map_err(|error| format!("end yes {}: {error}", child.id()))?;
            child
                .wait()
                .map_err(|error| format!("wait yes {}: {error}", child.id()))?;
        }
        self.children.clear();
        Ok(())
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        for child in &mut self.children {
            if let Err(error) = child.kill().and_then(|()| child.wait().map(|_| ())) {
                eprintln!(
                    "error: measure-program-redraw: end yes {}: {error}",
                    child.id()
                );
            }
        }
    }
}

/// The display frames of `time` at `rate`, rounded up.
fn frames(time: Duration, rate: u32) -> u64 {
    (time.as_nanos() * u128::from(rate)).div_ceil(1_000_000_000) as u64
}

fn report(program: &str, options: &Options, redraws: &[Redraw]) {
    let first = redraws
        .iter()
        .map(|redraw| redraw.first)
        .max()
        .unwrap_or_default();
    let gap = redraws
        .iter()
        .flat_map(|redraw| redraw.gaps.iter().copied())
        .max()
        .unwrap_or_default();
    let outputs: Vec<usize> = redraws.iter().map(|redraw| redraw.outputs).collect();
    let largest = first.max(gap);
    let at_rates: Vec<String> = options
        .refresh_rates
        .iter()
        .map(|rate| format!("{} frames at {rate} Hz", frames(largest, *rate)))
        .collect();
    println!(
        "{program} load={} repeats={} by={}: first output at most {:.1} ms, largest gap {:.1} ms, outputs per scroll {:?}; largest {:.1} ms = {}",
        options.load,
        redraws.len(),
        options.by,
        first.as_secs_f64() * 1000.0,
        gap.as_secs_f64() * 1000.0,
        outputs,
        largest.as_secs_f64() * 1000.0,
        at_rates.join(", ")
    );
}
