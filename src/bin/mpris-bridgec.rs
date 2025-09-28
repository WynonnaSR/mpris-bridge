use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct State {
    name: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    status: Option<String>,
    position: Option<f64>,
    length: Option<f64>,
}

fn runtime_dir() -> String {
    env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        let uid = nix::unistd::Uid::current().as_raw();
        format!("/run/user/{uid}")
    })
}

fn socket_path() -> PathBuf {
    PathBuf::from(format!("{}/mpris-bridge/mpris-bridge.sock", runtime_dir()))
}
fn state_path() -> PathBuf {
    PathBuf::from(format!("{}/mpris-bridge/state.json", runtime_dir()))
}
fn events_path() -> PathBuf {
    PathBuf::from(format!("{}/mpris-bridge/events.jsonl", runtime_dir()))
}

fn read_selected_from_state() -> Option<String> {
    let p = state_path();
    let txt = fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    v.get("name").and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn playerctl_exec(maybe_player: Option<String>, args: &[&str]) {
    let mut cmd = Command::new("playerctl");
    if let Some(p) = maybe_player {
        cmd.arg("-p").arg(p);
    }
    let _ = cmd.args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn send_over_socket(payload: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    Ok(())
}

fn usage() {
    eprintln!(
        "{}",
        r#"Usage:
  mpris-bridgec play-pause [--player <name>]
  mpris-bridgec next [--player <name>]
  mpris-bridgec previous [--player <name>]
  mpris-bridgec seek <offset-seconds> [--player <name>]
  mpris-bridgec set-position <seconds> [--player <name>]
  mpris-bridgec watch [--format <fmt>] [--truncate <n>] [--pango-escape]

watch defaults:
  --format "{artist}{sep}{title}"
  where sep = " - " if both artist & title are non-empty, else ""

--pango-escape   Escape Pango markup: & < > ' " → &amp; &lt; &gt; &apos; &quot;
"#
    );
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }

    // общий флаг --player для команд управления
    let mut player_arg: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--player" && i + 1 < args.len() {
            player_arg = Some(args.remove(i + 1));
            args.remove(i);
        } else {
            i += 1;
        }
    }

    let cmd = args.remove(0);
    match cmd.as_str() {
        "play-pause" | "next" | "previous" | "seek" | "set-position" => {
            run_control(cmd, player_arg, args);
        }
        "watch" => {
            run_watch(args);
        }
        _ => {
            usage();
            std::process::exit(2);
        }
    }
}

fn resolve_player(explicit: Option<String>) -> Option<String> {
    if explicit.is_some() {
        return explicit;
    }
    if let Some(sel) = read_selected_from_state() {
        return Some(sel);
    }
    None
}

fn run_control(cmd: String, player_arg: Option<String>, mut args: Vec<String>) {
    let mut socket_payload = None;
    let mut fallback: Option<(Option<String>, Vec<String>)> = None;

    match cmd.as_str() {
        "play-pause" => {
            socket_payload = Some(json!({"cmd":"play-pause","player":player_arg}).to_string());
            fallback = Some((resolve_player(player_arg), vec!["play-pause".into()]));
        }
        "next" => {
            socket_payload = Some(json!({"cmd":"next","player":player_arg}).to_string());
            fallback = Some((resolve_player(player_arg), vec!["next".into()]));
        }
        "previous" => {
            socket_payload = Some(json!({"cmd":"previous","player":player_arg}).to_string());
            fallback = Some((resolve_player(player_arg), vec!["previous".into()]));
        }
        "seek" => {
            if args.is_empty() {
                usage();
                std::process::exit(2);
            }
            let off = args[0].parse::<f64>().unwrap_or(0.0);
            socket_payload = Some(json!({"cmd":"seek","offset":off,"player":player_arg}).to_string());

            let sec = off.abs().round() as i64;
            let s = if off >= 0.0 { format!("{sec}+") } else { format!("{sec}-") };
            fallback = Some((resolve_player(player_arg), vec!["position".into(), s]));
        }
        "set-position" => {
            if args.is_empty() {
                usage();
                std::process::exit(2);
            }
            let pos = args[0].parse::<f64>().unwrap_or(0.0);
            socket_payload = Some(json!({"cmd":"set-position","position":pos,"player":player_arg}).to_string());

            let s = format!("{}", pos.round() as i64);
            fallback = Some((resolve_player(player_arg), vec!["position".into(), s]));
        }
        _ => unreachable!(),
    }

    if let Some(pay) = socket_payload {
        if send_over_socket(&pay).is_ok() {
            return;
        }
    }
    if let Some((maybe_player, argv)) = fallback {
        let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        playerctl_exec(maybe_player, &argv_ref);
    }
}

fn run_watch(mut args: Vec<String>) {
    // флаги: --format, --truncate, --pango-escape
    let mut format: Option<String> = None;
    let mut truncate: Option<usize> = None;
    let mut pango_escape = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" if i + 1 < args.len() => {
                format = Some(args.remove(i + 1));
                args.remove(i);
            }
            "--truncate" if i + 1 < args.len() => {
                truncate = args[i + 1].parse::<usize>().ok();
                args.drain(i..=i + 1);
            }
            "--pango-escape" => {
                pango_escape = true;
                args.remove(i);
            }
            _ => i += 1,
        }
    }

    // Выводим текущий снапшот
    if let Some(line) = compute_label_from_snapshot(format.as_deref(), truncate, pango_escape) {
        println!("{line}");
        std::io::stdout().flush().ok();
    }

    // Читаем events.jsonl и печатаем обновления
    follow_events_and_print(format.as_deref(), truncate, pango_escape);
}

fn compute_label_from_snapshot(fmt: Option<&str>, trunc: Option<usize>, pango: bool) -> Option<String> {
    let p = state_path();
    let txt = fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let artist = v.get("artist").and_then(|x| x.as_str()).unwrap_or("");
    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
    let line = format_label(artist, title, fmt, trunc);
    Some(if pango { pango_escape(&line) } else { line })
}

fn format_label(artist: &str, title: &str, fmt: Option<&str>, trunc: Option<usize>) -> String {
    let (artist_s, title_s) = (artist.to_string(), title.to_string());
    let sep = if !artist_s.is_empty() && !title_s.is_empty() { " - " } else { "" };
    let mut out = if let Some(f) = fmt {
        f.replace("{artist}", &artist_s).replace("{title}", &title_s).replace("{sep}", sep)
    } else {
        format!("{}{}{}", artist_s, sep, title_s)
    };
    if let Some(n) = trunc {
        if out.chars().count() > n {
            out = out.chars().take(n.saturating_sub(1)).collect::<String>() + "…";
        }
    }
    out
}

fn pango_escape(s: &str) -> String {
    // порядок важен: сначала & затем остальные
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

fn follow_events_and_print(fmt: Option<&str>, trunc: Option<usize>, pango: bool) {
    let path = events_path();
    let _ = OpenOptions::new().create(true).append(true).open(&path);

    loop {
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                thread::sleep(Duration::from_millis(300));
                continue;
            }
        };
        let mut reader = BufReader::new(file);
        let _ = reader.get_mut().seek(SeekFrom::End(0));

        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                        let artist = v.get("artist").and_then(|x| x.as_str()).unwrap_or("");
                        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
                        let mut out = format_label(artist, title, fmt, trunc);
                        if pango {
                            out = pango_escape(&out);
                        }
                        println!("{out}");
                        let _ = std::io::stdout().flush();
                    }
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }
    }
}