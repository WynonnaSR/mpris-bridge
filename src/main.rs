//! mpris-bridge 0.3.x: Event-driven MPRIS state for Waybar/Eww
//! - Selection by D-Bus signals (zbus 3.x) + Hyprland focus, no periodic reselect timers.
//! - Single follower (playerctl -F) for the selected player to fetch metadata/position/art.
//! - JSON output compatible with your eww/Waybar (camelCase).
//! - Lightweight IPC over Unix socket for media controls (play-pause/next/previous/seek).
//!
//! Notes:
//! - We use MessageStream to receive signals and cheap "seed" via playerctl when needed.
//! - No unsafe. Avoid holding locks across awaits. Futures are Send.
//! - For IPC we use blocking std::os::unix sockets on a dedicated blocking task; no extra tokio features needed.

#![deny(unsafe_code)]
#![deny(clippy::all, clippy::pedantic, clippy::nursery, clippy::perf)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::too_many_lines
)]

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    signal::unix::{signal, SignalKind},
    sync::watch,
    task,
    time::Instant, // <-- добавлено: используем для дебаунса
};
use zbus::{fdo::DBusProxy, Connection, MessageStream, MessageType};

// ------------------------- Config -------------------------

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    selection: Selection,
    #[serde(default)]
    art: Art,
    #[serde(default)]
    output: Output,
    #[serde(default)]
    presentation: Presentation,
    #[allow(dead_code)]
    #[serde(default)]
    logging: Logging,
}

#[derive(Debug, Deserialize)]
struct Selection {
    #[serde(default = "default_priority")]
    priority: Vec<String>,
    #[serde(default = "dtrue")]
    remember_last: bool,
    #[serde(default = "fallback_any")]
    fallback: String, // "any" | "none"
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}
fn default_priority() -> Vec<String> {
    vec!["firefox".into(), "spotify".into(), "vlc".into(), "mpv".into()]
}
fn dtrue() -> bool {
    true
}
fn fallback_any() -> String {
    "any".into()
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            priority: default_priority(),
            remember_last: true,
            fallback: "any".into(),
            include: vec![],
            exclude: vec![],
        }
    }
}

#[derive(Debug, Deserialize)]
struct Art {
    #[serde(default = "dtrue")]
    enabled: bool,
    #[serde(default = "dtrue")]
    download_http: bool,
    #[serde(default = "d5000")]
    timeout_ms: u64,
    #[serde(default)]
    cache_dir: Option<String>,
    #[serde(default)]
    default_image: Option<String>,
    #[serde(default)]
    current_path: Option<String>,
    #[serde(default)]
    use_symlink: bool,
}
fn d5000() -> u64 {
    5000
}
impl Default for Art {
    fn default() -> Self {
        Self {
            enabled: true,
            download_http: true,
            timeout_ms: d5000(),
            cache_dir: None,
            default_image: None,
            current_path: None,
            use_symlink: false,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Output {
    #[serde(default)]
    snapshot_path: Option<String>,
    #[serde(default)]
    events_path: Option<String>,
    #[serde(default)]
    pretty_snapshot: bool,
}
impl Default for Output {
    fn default() -> Self {
        Self {
            snapshot_path: None,
            events_path: None,
            pretty_snapshot: false,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Presentation {
    #[serde(default = "d120usize")]
    truncate_title: usize,
    #[serde(default = "d120usize")]
    truncate_artist: usize,
}
fn d120usize() -> usize {
    120
}
impl Default for Presentation {
    fn default() -> Self {
        Self {
            truncate_title: d120usize(),
            truncate_artist: d120usize(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Logging {
    #[allow(dead_code)]
    #[serde(default = "default_level")]
    level: String,
}
fn default_level() -> String {
    "warn".into()
}
impl Default for Logging {
    fn default() -> Self {
        Self {
            level: default_level(),
        }
    }
}

// ------------------------- Model/State -------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiState {
    name: String,
    title: String,
    artist: String,
    status: String,
    position: f64,
    position_str: String,
    length: f64,
    length_str: String,
    thumbnail: String,
    can_next: i32,
    can_prev: i32,
}
impl UiState {
    fn empty(default_cover: &str) -> Self {
        Self {
            name: String::new(),
            title: String::new(),
            artist: String::new(),
            status: String::new(),
            position: 0.0,
            position_str: fmt_time(0.0),
            length: 0.0,
            length_str: fmt_time(0.0),
            thumbnail: default_cover.to_string(),
            can_next: 0,
            can_prev: 0,
        }
    }
}

#[derive(Debug)]
struct Ctx {
    cfg: Config,
    cache_dir: PathBuf,
    default_cover: PathBuf,
    current_cover: PathBuf,
    snapshot_path: PathBuf,
    events_path: PathBuf,

    // Known players and their statuses
    players: RwLock<HashSet<String>>,        // simple names like "firefox.instance_1_240"
    status: RwLock<HashMap<String, String>>, // "Playing"/"Paused"/"Stopped"

    // Selection & focus
    selected: RwLock<Option<String>>,
    last_selected: RwLock<Option<String>>,
    focus_hint: RwLock<Option<String>>, // "firefox"/"spotify"/...

    // Follower process flag
    follower_alive: AtomicBool,

    // Notify follower manager on selection changes
    sel_tx: watch::Sender<Option<String>>,
}
impl Ctx {
    fn new(cfg: Config, sel_tx: watch::Sender<Option<String>>) -> Self {
        let cache_dir =
            PathBuf::from(expand(cfg.art.cache_dir.as_deref().unwrap_or("$XDG_CACHE_HOME/mpris-bridge/art")));
        let default_cover = PathBuf::from(expand(
            cfg.art
                .default_image
                .as_deref()
                .unwrap_or("$HOME/.config/eww/scripts/cover.png"),
        ));
        let current_cover = PathBuf::from(expand(
            cfg.art
                .current_path
                .as_deref()
                .unwrap_or("$HOME/.config/eww/image.jpg"),
        ));
        let snapshot_path = PathBuf::from(expand(
            cfg.output
                .snapshot_path
                .as_deref()
                .unwrap_or("$XDG_RUNTIME_DIR/mpris-bridge/state.json"),
        ));
        let events_path = PathBuf::from(expand(
            cfg.output
                .events_path
                .as_deref()
                .unwrap_or("$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl"),
        ));
        Self {
            cfg,
            cache_dir,
            default_cover,
            current_cover,
            snapshot_path,
            events_path,
            players: RwLock::new(HashSet::new()),
            status: RwLock::new(HashMap::new()),
            selected: RwLock::new(None),
            last_selected: RwLock::new(None),
            focus_hint: RwLock::new(None),
            follower_alive: AtomicBool::new(false),
            sel_tx,
        }
    }
}

// ------------------------- Utils -------------------------

fn fmt_time(s: f64) -> String {
    let secs = s.max(0.0).floor() as i64;
    let m = secs / 60;
    let r = secs % 60;
    format!("{m}:{r:02}")
}

fn expand(path: &str) -> String {
    let mut s = path.to_string();
    if let Some(home) = dirs::home_dir() {
        s = s.replace("$HOME", home.to_string_lossy().as_ref());
    }
    if let Some(cfg) = dirs::config_dir() {
        s = s.replace("$XDG_CONFIG_HOME", cfg.to_string_lossy().as_ref());
    }
    if let Some(cache) = dirs::cache_dir() {
        s = s.replace("$XDG_CACHE_HOME", cache.to_string_lossy().as_ref());
    }
    if let Ok(run) = std::env::var("XDG_RUNTIME_DIR") {
        s = s.replace("$XDG_RUNTIME_DIR", &run);
    } else {
        let uid = nix::unistd::Uid::current().as_raw();
        s = s.replace("$XDG_RUNTIME_DIR", &format!("/run/user/{uid}"));
    }
    s
}

fn ensure_dirs(ctx: &Ctx) {
    if let Some(p) = ctx.snapshot_path.parent() {
        let _ = fs::create_dir_all(p);
    }
    if let Some(p) = ctx.events_path.parent() {
        let _ = fs::create_dir_all(p);
    }
    if let Some(p) = ctx.current_cover.parent() {
        let _ = fs::create_dir_all(p);
    }
    let _ = fs::create_dir_all(&ctx.cache_dir);
}

fn include_exclude_match(name: &str, include: &[String], exclude: &[String]) -> bool {
    if !include.is_empty() && !include.iter().any(|x| name.starts_with(x)) {
        return false;
    }
    if !exclude.is_empty() && exclude.iter().any(|x| name.starts_with(x)) {
        return false;
    }
    true
}

fn map_class_to_hint(class: &str) -> Option<String> {
    let lc = class.to_lowercase();
    if lc.starts_with("firefox") {
        Some("firefox".into())
    } else if lc.starts_with("spotify") {
        Some("spotify".into())
    } else if lc.starts_with("vlc") {
        Some("vlc".into())
    } else if lc.starts_with("mpv") {
        Some("mpv".into())
    } else {
        None
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

// ------------------------- JSON I/O -------------------------

async fn write_state(ctx: &Ctx, st: &UiState) -> Result<()> {
    // snapshot (atomic)
    let json =
        if ctx.cfg.output.pretty_snapshot { serde_json::to_string_pretty(st)? } else { serde_json::to_string(st)? };
    let tmp = ctx.snapshot_path.with_extension("json.tmp");
    fs::write(&tmp, json.as_bytes())?;
    fs::rename(&tmp, &ctx.snapshot_path)?;
    // events (append)
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ctx.events_path)?;
    let line = serde_json::to_string(st)?;
    writeln!(f, "{line}")?;
    Ok(())
}

// ------------------------- Cover Art -------------------------

async fn update_art(ctx: &Ctx, art_url: &str) -> Result<String> {
    if !ctx.cfg.art.enabled {
        return Ok(ctx.current_cover.to_string_lossy().to_string());
    }
    let file_re = Regex::new(r"^file://").unwrap();
    let http_re = Regex::new(r"^https?://").unwrap();

    if file_re.is_match(art_url) {
        let local_path = art_url.trim_start_matches("file://");
        if Path::new(local_path).is_file() {
            ensure_current_cover(ctx, Path::new(local_path))?;
            return Ok(ctx.current_cover.to_string_lossy().to_string());
        }
    } else if http_re.is_match(art_url) && ctx.cfg.art.download_http {
        let mut hasher = Sha1::new();
        hasher.update(art_url.as_bytes());
        let fname = format!("{:x}", hasher.finalize());
        let target = ctx.cache_dir.join(format!("{fname}.jpg"));
        if !target.exists() {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(ctx.cfg.art.timeout_ms))
                .build()?;
            let resp = client.get(art_url).send().await?;
            if resp.status().is_success() {
                let bytes = resp.bytes().await.unwrap_or(Bytes::new());
                if !bytes.is_empty() {
                    fs::write(&target, &bytes)?;
                }
            }
        }
        if target.exists() {
            ensure_current_cover(ctx, &target)?;
            return Ok(ctx.current_cover.to_string_lossy().to_string());
        }
    }

    ensure_current_cover(ctx, &ctx.default_cover)?;
    Ok(ctx.current_cover.to_string_lossy().to_string())
}

fn ensure_current_cover(ctx: &Ctx, src: &Path) -> Result<()> {
    if let Some(p) = ctx.current_cover.parent() {
        let _ = fs::create_dir_all(p);
    }
    if ctx.cfg.art.use_symlink {
        if ctx.current_cover.exists() {
            let _ = fs::remove_file(&ctx.current_cover);
        }
        #[allow(clippy::let_underscore_must_use)]
        let _ = std::os::unix::fs::symlink(src, &ctx.current_cover);
    } else {
        #[allow(clippy::let_underscore_must_use)]
        let _ = fs::copy(src, &ctx.current_cover);
    }
    Ok(())
}

// ------------------------- Selection -------------------------

fn recompute_selected(ctx: &Ctx) -> Option<String> {
    let include = &ctx.cfg.selection.include;
    let exclude = &ctx.cfg.selection.exclude;
    let priority = &ctx.cfg.selection.priority;

    let players: Vec<String> = ctx
        .players
        .read()
        .unwrap()
        .iter()
        .filter(|p| include_exclude_match(p, include, exclude))
        .cloned()
        .collect();

    if players.is_empty() {
        return None;
    }

    let status_map = ctx.status.read().unwrap().clone();
    let mut playing: Vec<String> = players
        .iter()
        .filter(|p| status_map.get(*p).map_or(false, |s| s == "Playing"))
        .cloned()
        .collect();

    let focus = ctx.focus_hint.read().unwrap().clone();

    if !playing.is_empty() {
        if let Some(f) = &focus {
            if let Some(p) = playing.iter().find(|pp| pp.starts_with(f)) {
                return Some(p.clone());
            }
        }
        for want in priority {
            if let Some(p) = playing.iter().find(|pp| pp.starts_with(want)) {
                return Some(p.clone());
            }
        }
        return Some(playing.remove(0));
    }

    if ctx.cfg.selection.remember_last {
        if let Some(last) = ctx.last_selected.read().unwrap().clone() {
            if players.iter().any(|p| *p == last) {
                return Some(last);
            }
        }
    }
    if let Some(f) = &focus {
        if let Some(p) = players.iter().find(|pp| pp.starts_with(f)) {
            return Some(p.clone());
        }
    }
    for want in priority {
        if let Some(p) = players.iter().find(|pp| pp.starts_with(want)) {
            return Some(p.clone());
        }
    }
    if ctx.cfg.selection.fallback == "any" {
        return Some(players[0].clone());
    }
    None
}

// Set selection; returns true if changed, and notifies follower manager via watch channel.
fn set_selected_sync(ctx: &Ctx, name: Option<String>) -> bool {
    let mut sel = ctx.selected.write().unwrap();
    let changed = *sel != name;
    *sel = name.clone();
    if let Some(n) = name {
        *ctx.last_selected.write().unwrap() = Some(n);
    }
    if changed {
        let _ = ctx.sel_tx.send(sel.clone());
    }
    changed
}

// Recompute selection and if changed, send quick snapshot immediately.
fn set_selected_and_kick(ctx: &Arc<Ctx>, name: Option<String>) {
    let changed = set_selected_sync(ctx, name.clone());
    if changed {
        if let Some(n) = name {
            let ctx2 = ctx.clone();
            task::spawn(async move { emit_quick_snapshot(ctx2, n).await; });
        }
    }
}

// ------------------------- Follower (playerctl -F) -------------------------

// Read capabilities (CanGoNext/Previous) once per track/status change (via busctl; cheap).
async fn get_caps_dbus(simple_name: &str) -> (i32, i32) {
    let busname = format!("org.mpris.MediaPlayer2.{simple_name}");
    let outn = Command::new("busctl")
        .arg("--user")
        .arg("get-property")
        .arg(&busname)
        .arg("/org/mpris/MediaPlayer2")
        .arg("org.mpris.MediaPlayer2.Player")
        .arg("CanGoNext")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;
    let outp = Command::new("busctl")
        .arg("--user")
        .arg("get-property")
        .arg(&busname)
        .arg("/org/mpris/MediaPlayer2")
        .arg("org.mpris.MediaPlayer2.Player")
        .arg("CanGoPrevious")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    let s_n = outn
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let s_p = outp
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    (i32::from(s_n.contains("b true")), i32::from(s_p.contains("b true")))
}

// Override policy for YouTube in Firefox: no playlist => only next enabled.
fn override_caps_for_youtube(simple_name: &str, url: &str, can_next: i32, can_prev: i32) -> (i32, i32) {
    let is_firefox = simple_name.starts_with("firefox");
    let is_yt = url.contains("youtube.com/watch") || url.contains("music.youtube.com");
    if is_firefox && is_yt {
        let has_playlist = url.contains("list=");
        if !has_playlist {
            return (1, 0);
        }
    }
    (can_next, can_prev)
}

async fn spawn_follower(ctx: Arc<Ctx>, name: String) -> Result<Child> {
    // Initial blank snapshot with name (instant UI switch)
    {
        let mut st = UiState::empty(&ctx.default_cover.to_string_lossy());
        st.name = name.clone();
        write_state(&ctx, &st).await?;
    }

    let mut child = Command::new("playerctl")
        .arg("-p")
        .arg(&name)
        .arg("metadata")
        .arg("--format")
        .arg("{{status}}|{{playerName}}|{{title}}|{{artist}}|{{mpris:length}}|{{mpris:artUrl}}|{{position}}|{{xesam:url}}")
        .arg("-F")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn playerctl -F")?;

    let stdout = child.stdout.take().context("follower stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    ctx.follower_alive.store(true, Ordering::SeqCst);

    let ctx_clone = ctx.clone();
    let name_clone = name.clone();
    task::spawn(async move {
        // Local buffers to avoid excess busctl calls
        let mut last_status = String::new();
        let mut last_title = String::new();
        let mut last_artist = String::new();
        let mut last_url = String::new();
        let mut last_can_next = 0;
        let mut last_can_prev = 0;

        while let Ok(Some(l)) = lines.next_line().await {
            let parts: Vec<_> = l.splitn(8, '|').map(|s| s.trim().to_string()).collect();
            if parts.len() != 8 {
                continue;
            }

            let status = parts[0].clone();
            let title = parts[2].clone();
            let artist = parts[3].clone();
            let len_us = parts[4].clone();
            let art = parts[5].clone();
            let pos_us = parts[6].clone(); // microseconds
            let url = parts[7].clone();

            // Update status map (helps selection policy)
            {
                ctx_clone
                    .status
                    .write()
                    .unwrap()
                    .insert(name_clone.clone(), status.clone());
            }

            // Capabilities refresh on meaningful changes
            let mut can_next = last_can_next;
            let mut can_prev = last_can_prev;
            if status != last_status || title != last_title || artist != last_artist || url != last_url {
                let (n, p) = get_caps_dbus(&name_clone).await;
                let (n, p) = override_caps_for_youtube(&name_clone, &url, n, p);
                can_next = n;
                can_prev = p;
                last_can_next = n;
                last_can_prev = p;
                last_status = status.clone();
                last_title = title.clone();
                last_artist = artist.clone();
                last_url = url.clone();
            }

            let mut st = UiState::empty(&ctx_clone.default_cover.to_string_lossy());
            st.name = name_clone.clone();
            st.status = status;
            st.title = truncate(&title, ctx_clone.cfg.presentation.truncate_title);
            st.artist = truncate(&artist, ctx_clone.cfg.presentation.truncate_artist);

            if let Ok(us) = len_us.parse::<u64>() {
                st.length = (us as f64) / 1_000_000.0;
                st.length_str = fmt_time(st.length);
            }

            // Position fix: µs → s
            if let Ok(usf) = pos_us.parse::<f64>() {
                let pos = usf / 1_000_000.0;
                st.position = pos;
                st.position_str = fmt_time(pos);
            }

            st.thumbnail = update_art(&ctx_clone, &art)
                .await
                .unwrap_or_else(|_| ctx_clone.default_cover.to_string_lossy().to_string());

            st.can_next = can_next;
            st.can_prev = can_prev;

            if let Err(e) = write_state(&ctx_clone, &st).await {
                eprintln!("mpris-bridge: write_state error: {e:#}");
            }
        }
        ctx_clone.follower_alive.store(false, Ordering::SeqCst);
    });

    Ok(child)
}

// Watchdog + reactive follower manager
async fn follower_manager(ctx: Arc<Ctx>, mut rx: watch::Receiver<Option<String>>) -> Result<()> {
    use tokio::time::interval;
    let mut current: Option<String> = None;
    let mut child_opt: Option<Child> = None;
    let mut tick = interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = rx.changed() => {
                let desired = rx.borrow().clone();
                if desired != current {
                    if let Some(mut ch) = child_opt.take() {
                        let _ = ch.kill().await;
                    }
                    if let Some(name) = desired.clone() {
                        match spawn_follower(ctx.clone(), name).await {
                            Ok(child) => { child_opt = Some(child); }
                            Err(e) => eprintln!("mpris-bridge: spawn follower failed: {e:#}"),
                        }
                    }
                    current = desired;
                }
            }
            _ = tick.tick() => {
                // Watchdog: selected exists but follower not alive -> respawn
                let selected = ctx.selected.read().unwrap().clone();
                let alive = ctx.follower_alive.load(Ordering::SeqCst);
                if selected.is_some() && !alive {
                    if let Some(mut ch) = child_opt.take() {
                        let _ = ch.kill().await;
                    }
                    if let Some(name) = selected.clone() {
                        match spawn_follower(ctx.clone(), name).await {
                            Ok(child) => { child_opt = Some(child); }
                            Err(e) => eprintln!("mpris-bridge: respawn follower failed: {e:#}"),
                        }
                    }
                    current = selected;
                }
            }
        }
    }
}

// ------------------------- Quick snapshot on selection change -------------------------

async fn emit_quick_snapshot(ctx: Arc<Ctx>, name: String) {
    // One-shot metadata for instant UI refresh on selection switch
    let out = Command::new("playerctl")
        .arg("-p")
        .arg(&name)
        .arg("metadata")
        .arg("--format")
        .arg("{{status}}|{{playerName}}|{{title}}|{{artist}}|{{mpris:length}}|{{mpris:artUrl}}|{{position}}|{{xesam:url}}")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    let Ok(o) = out else { return; };
    let s = String::from_utf8_lossy(&o.stdout);
    let parts: Vec<_> = s.trim().splitn(8, '|').map(|x| x.to_string()).collect();
    if parts.len() != 8 {
        return;
    }

    let status = parts[0].clone();
    let title = parts[2].clone();
    let artist = parts[3].clone();
    let len_us = parts[4].clone();
    let art = parts[5].clone();
    let pos_us = parts[6].clone();
    let url = parts[7].clone();

    {
        ctx.status
            .write()
            .unwrap()
            .insert(name.clone(), status.clone());
    }

    let (n, p) = get_caps_dbus(&name).await;
    let (n, p) = override_caps_for_youtube(&name, &url, n, p);

    let mut st = UiState::empty(&ctx.default_cover.to_string_lossy());
    st.name = name.clone();
    st.status = status;
    st.title = truncate(&title, ctx.cfg.presentation.truncate_title);
    st.artist = truncate(&artist, ctx.cfg.presentation.truncate_artist);

    if let Ok(us) = len_us.parse::<u64>() {
        st.length = (us as f64) / 1_000_000.0;
        st.length_str = fmt_time(st.length);
    }
    if let Ok(usf) = pos_us.parse::<f64>() {
        let pos = usf / 1_000_000.0;
        st.position = pos;
        st.position_str = fmt_time(pos);
    }

    st.thumbnail = update_art(&ctx, &art)
        .await
        .unwrap_or_else(|_| ctx.default_cover.to_string_lossy().to_string());
    st.can_next = n;
    st.can_prev = p;

    let _ = write_state(&ctx, &st).await;
}

// ------------------------- IPC (Unix socket) -------------------------

use serde::Deserialize as De;
#[derive(Debug, De)]
#[serde(tag = "cmd")]
enum IpcCmd {
    #[serde(rename = "play-pause")]
    PlayPause { player: Option<String> },
    #[serde(rename = "next")]
    Next { player: Option<String> },
    #[serde(rename = "previous")]
    Previous { player: Option<String> },
    #[serde(rename = "seek")]
    Seek { offset: f64, player: Option<String> }, // seconds (+/-)
    #[serde(rename = "set-position")]
    SetPosition { position: f64, player: Option<String> }, // seconds (absolute)
}

fn pick_player_sync(ctx: &Ctx, explicit: &Option<String>) -> Option<String> {
    if let Some(p) = explicit.clone() {
        return Some(p);
    }
    ctx.selected.read().unwrap().clone()
}

fn run_playerctl_cmd_sync(player: &str, args: &[&str]) {
    let _ = std::process::Command::new("playerctl")
        .arg("-p")
        .arg(player)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn handle_ipc_stream_blocking(ctx: Arc<Ctx>, mut stream: UnixStream) {
    use std::io::{BufRead, BufReader, Write};
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n == 0 {
            break;
        }
        let txt = line.trim();
        if txt.is_empty() {
            continue;
        }
        let mut ok = true;
        if let Ok(cmd) = serde_json::from_str::<IpcCmd>(txt) {
            match cmd {
                IpcCmd::PlayPause { player } => {
                    if let Some(p) = pick_player_sync(&ctx, &player) {
                        run_playerctl_cmd_sync(&p, &["play-pause"]);
                    } else {
                        ok = false;
                    }
                }
                IpcCmd::Next { player } => {
                    if let Some(p) = pick_player_sync(&ctx, &player) {
                        run_playerctl_cmd_sync(&p, &["next"]);
                    } else {
                        ok = false;
                    }
                }
                IpcCmd::Previous { player } => {
                    if let Some(p) = pick_player_sync(&ctx, &player) {
                        run_playerctl_cmd_sync(&p, &["previous"]);
                    } else {
                        ok = false;
                    }
                }
                IpcCmd::Seek { offset, player } => {
                    if let Some(p) = pick_player_sync(&ctx, &player) {
                        // playerctl position takes "5+" or "5-"
                        let s = if offset >= 0.0 {
                            format!("{}+", offset as i64)
                        } else {
                            format!("{}-", (-offset) as i64)
                        };
                        run_playerctl_cmd_sync(&p, &["position", &s]);
                    } else {
                        ok = false;
                    }
                }
                IpcCmd::SetPosition { position, player } => {
                    if let Some(p) = pick_player_sync(&ctx, &player) {
                        let s = format!("{}", position as i64);
                        run_playerctl_cmd_sync(&p, &["position", &s]);
                    } else {
                        ok = false;
                    }
                }
            }
        } else {
            ok = false;
        }

        let _ = if ok {
            write!(stream, "{{\"ok\":true}}\n")
        } else {
            write!(stream, "{{\"ok\":false}}\n")
        };
        let _ = stream.flush();
    }
}

fn ipc_server_blocking(ctx: Arc<Ctx>) -> std::io::Result<()> {
    // $XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let dir = format!("{base}/mpris-bridge");
    let sock = format!("{dir}/mpris-bridge.sock");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    let _ = fs::set_permissions(&sock, fs::Permissions::from_mode(0o600));

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let ctx2 = ctx.clone();
                std::thread::spawn(move || {
                    handle_ipc_stream_blocking(ctx2, stream);
                });
            }
            Err(e) => {
                eprintln!("mpris-bridge: ipc accept error: {e:#}");
            }
        }
    }
    Ok(())
}

// ------------------------- D-Bus (zbus) + Hypr focus -------------------------

// Reconnecting wrapper with backoff
async fn dbus_listener(ctx: Arc<Ctx>) -> Result<()> {
    use tokio::time::sleep;
    let mut backoff_secs: u64 = 1;

    loop {
        match dbus_main_loop(ctx.clone()).await {
            Ok(()) => {
                // Graceful end, small delay and restart
                sleep(Duration::from_millis(500)).await;
                backoff_secs = 1;
            }
            Err(e) => {
                eprintln!("mpris-bridge: dbus loop error: {e:#} (will reconnect)");
                let delay = (backoff_secs.min(30)) * 200;
                sleep(Duration::from_millis(delay)).await;
                backoff_secs = (backoff_secs.saturating_mul(2)).min(30);
            }
        }
    }
}

// Single DBus session: connect, subscribe and process
async fn dbus_main_loop(ctx: Arc<Ctx>) -> Result<()> {
    let conn = Connection::session().await.context("dbus session")?;

    // Сузить подписки: только MPRIS-плееры и их свойства на стандартном пути.
    let dbus = DBusProxy::new(&conn).await?;
    // Смена владельцев ТОЛЬКО для имён в пространстве org.mpris.MediaPlayer2.*
    dbus.add_match("type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'")
        .await?;
    // Изменения свойств ТОЛЬКО на /org/mpris/MediaPlayer2 для интерфейса Player
    dbus.add_match("type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/mpris/MediaPlayer2',arg0='org.mpris.MediaPlayer2.Player'")
        .await?;
    // И (реже) для корневого интерфейса org.mpris.MediaPlayer2 (необязательно, но полезно)
    dbus.add_match("type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/mpris/MediaPlayer2',arg0='org.mpris.MediaPlayer2'")
        .await?;

    let mut stream = MessageStream::from(&conn);

    // Seed players & statuses once, and select initial player
    seed_players(&ctx).await?;
    let init_sel = recompute_selected(&ctx);
    set_selected_and_kick(&ctx, init_sel);

    // Дебаунс тяжёлых операций, выполняем в фоновых задачах
    let mut last_seed = Instant::now() - Duration::from_secs(3600);
    let mut last_refresh = Instant::now() - Duration::from_secs(3600);
    const SEED_DEBOUNCE_MS: u64 = 300;
    const REFRESH_DEBOUNCE_MS: u64 = 250;

    // React to bus signals
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let Ok(hdr) = msg.header() else { continue; };
        if !matches!(hdr.message_type(), Ok(MessageType::Signal)) {
            continue;
        }

        let iface = hdr.interface().ok().flatten().map(|i| i.as_str().to_string());
        let member = hdr.member().ok().flatten().map(|m| m.as_str().to_string());
        let path = hdr.path().ok().flatten().map(|p| p.as_str().to_string());

        match (iface.as_deref(), member.as_deref()) {
            (Some("org.freedesktop.DBus"), Some("NameOwnerChanged")) => {
                // Уже отфильтровано по arg0namespace='org.mpris.MediaPlayer2'
                if last_seed.elapsed() >= Duration::from_millis(SEED_DEBOUNCE_MS) {
                    last_seed = Instant::now();
                    let ctx2 = ctx.clone();
                    task::spawn(async move {
                        if let Err(e) = seed_players(&ctx2).await {
                            eprintln!("mpris-bridge: seed on NameOwnerChanged failed: {e:#}");
                            return;
                        }
                        let new_sel = recompute_selected(&ctx2);
                        set_selected_and_kick(&ctx2, new_sel);
                    });
                }
            }
            (Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged")) => {
                // Уже отфильтровано: path='/org/mpris/MediaPlayer2' и arg0 в add_match
                if path.as_deref() != Some("/org/mpris/MediaPlayer2") {
                    continue;
                }
                if last_refresh.elapsed() >= Duration::from_millis(REFRESH_DEBOUNCE_MS) {
                    last_refresh = Instant::now();
                    let ctx2 = ctx.clone();
                    task::spawn(async move {
                        if let Err(e) = refresh_statuses(&ctx2).await {
                            eprintln!("mpris-bridge: refresh statuses failed: {e:#}");
                        }
                        let new_sel = recompute_selected(&ctx2);
                        set_selected_and_kick(&ctx2, new_sel);
                    });
                }
            }
            _ => {}
        }
    }

    Ok(())
}

// Restarting hyprctl -i events on exit
async fn hypr_focus_listener(ctx: Arc<Ctx>) -> Result<()> {
    use tokio::time::sleep;
    loop {
        let mut child = match Command::new("hyprctl")
            .arg("-i")
            .arg("events")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("mpris-bridge: hyprctl spawn error: {e:#}");
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                eprintln!("mpris-bridge: hyprctl no stdout");
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut lines = BufReader::new(stdout).lines();

        while let Some(line) = lines.next_line().await? {
            if line.starts_with("activewindow>>") {
                let out = Command::new("hyprctl")
                    .arg("activewindow")
                    .arg("-j")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .await?;
                if !out.stdout.is_empty() {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                        if let Some(class) = v.get("class").and_then(|x| x.as_str()) {
                            let hint = map_class_to_hint(class);
                            {
                                *ctx.focus_hint.write().unwrap() = hint;
                            }
                            let new_sel = recompute_selected(&ctx);
                            set_selected_and_kick(&ctx, new_sel);
                        }
                    }
                }
            }
        }

        // stream ended; wait and restart
        sleep(Duration::from_secs(1)).await;
    }
}

// ------------------------- Seed/Refresh -------------------------

async fn seed_players(ctx: &Arc<Ctx>) -> Result<()> {
    let out = Command::new("playerctl")
        .arg("-l")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .context("playerctl -l")?;
    let list = String::from_utf8_lossy(&out.stdout);
    let mut ps = HashSet::new();
    for line in list.lines() {
        let name = line.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if include_exclude_match(
            &name,
            &ctx.cfg.selection.include,
            &ctx.cfg.selection.exclude,
        ) {
            ps.insert(name);
        }
    }
    *ctx.players.write().unwrap() = ps;
    refresh_statuses(ctx).await?;
    Ok(())
}

async fn refresh_statuses(ctx: &Arc<Ctx>) -> Result<()> {
    let players: Vec<String> = ctx.players.read().unwrap().iter().cloned().collect();
    let mut st = HashMap::new();
    for p in players {
        let out = Command::new("playerctl")
            .arg("-p")
            .arg(&p)
            .arg("status")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            st.insert(p, s);
        }
    }
    *ctx.status.write().unwrap() = st;
    Ok(())
}

// ------------------------- Config I/O -------------------------

async fn read_config() -> Result<Config> {
    let cfg_dir = dirs::config_dir().context("no XDG_CONFIG_HOME")?;
    let path = cfg_dir.join("mpris-bridge").join("config.toml");
    let text = fs::read_to_string(&path).with_context(|| format!("reading config {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).context("parsing toml")?;
    Ok(cfg)
}

// ------------------------- Main -------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = read_config().await?;
    let (sel_tx, sel_rx) = watch::channel::<Option<String>>(None);
    let ctx = Arc::new(Ctx::new(cfg, sel_tx.clone()));
    ensure_dirs(&ctx);

    // Initial blank snapshot
    let init = UiState::empty(&ctx.default_cover.to_string_lossy());
    write_state(&ctx, &init).await?;

    // SIGHUP placeholder (hot-reload can be added later)
    let _ctx_for_signal = ctx.clone();
    task::spawn(async move {
        if let Ok(mut hup) = signal(SignalKind::hangup()) {
            while hup.recv().await.is_some() {
                eprintln!("mpris-bridge: SIGHUP received (reload TBD)");
            }
        }
    });

    // Follower manager (spawn/kill playerctl -F on selection changes) + watchdog
    let fm_ctx = ctx.clone();
    task::spawn(async move {
        if let Err(e) = follower_manager(fm_ctx, sel_rx).await {
            eprintln!("mpris-bridge: follower manager error: {e:#}");
        }
    });

    // IPC server (blocking Unix socket on a dedicated thread pool task)
    let ipc_ctx = ctx.clone();
    task::spawn_blocking(move || {
        if let Err(e) = ipc_server_blocking(ipc_ctx) {
            eprintln!("mpris-bridge: ipc server error: {e:#}");
        }
    });

    // Hyprland focus listener with self-restart
    let focus_ctx = ctx.clone();
    task::spawn(async move {
        if let Err(e) = hypr_focus_listener(focus_ctx).await {
            eprintln!("mpris-bridge: hypr focus listener failed: {e:#}");
        }
    });

    // D-Bus events listener with autoreconnect
    if let Err(e) = dbus_listener(ctx.clone()).await {
        eprintln!("mpris-bridge: dbus listener failed: {e:#}");
    }

    Ok(())
}