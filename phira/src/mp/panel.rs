use crate::{
    client::{Chart, Ptr, UserManager},
    dir, get_data,
    mp::L10N_LOCAL,
    scene::{compress_folder, Downloading, SongScene, LOCAL_MP_RESULT, RECORD_ID},
};
use anyhow::{anyhow, bail, Context, Result};
use inputbox::InputBox;
use macroquad::prelude::*;
use phira_mp_client::Client;
use phira_mp_common::{RoomId, RoomState};
use prpr::{
    config::Mods,
    core::{Smooth, Tweenable},
    ext::{poll_future, semi_black, semi_white, LocalTask, RectExt, SafeTexture},
    info::ChartInfo,
    scene::{request_input, return_input, show_error, show_message, take_input, GameMode, NextScene},
    task::Task,
    time::TimeManager,
    ui::{DRectButton, DrawText},
    ui::{Scroll, Ui},
};
use sha2::{Digest, Sha256};
use smallvec::SmallVec;
use std::{
    fs::{self, File},
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
};
use tracing::warn;
use uuid::Uuid;

const CACHE_MANIFEST: &str = ".mp-install-v2.json";

fn validate_chart_files(root: &Path) -> Result<()> {
    let info: ChartInfo = serde_yaml::from_reader(File::open(root.join("info.yml"))?)?;
    for name in [&info.chart, &info.music, &info.illustration].into_iter().chain(info.unlock_video.iter()) {
        let path = Path::new(name);
        anyhow::ensure!(
            !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, std::path::Component::Normal(_))),
            "unsafe chart file reference"
        );
        anyhow::ensure!(root.join(path).is_file(), "chart file missing: {name}");
    }
    Ok(())
}

/// Only our atomic installer writes this manifest, after validating all files.
fn cache_manifest(root: &Path) -> Result<Vec<(String, u64, String)>> {
    fn walk(root: &Path, dir: &Path, entries: &mut Vec<(String, u64, String)>, total: &mut u64) -> Result<()> {
        anyhow::ensure!(dir.strip_prefix(root)?.components().count() <= 64, "chart nesting too deep");
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            anyhow::ensure!(!metadata.file_type().is_symlink(), "symlink in chart cache");
            if metadata.is_dir() {
                walk(root, &path, entries, total)?;
            } else if metadata.is_file() && path != root.join(CACHE_MANIFEST) {
                *total = total.checked_add(metadata.len()).ok_or_else(|| anyhow!("chart cache too large"))?;
                anyhow::ensure!(*total <= 128 * 1024 * 1024 && entries.len() < 4096, "chart cache too large");
                let mut file = File::open(&path)?;
                let mut hasher = Sha256::new();
                let mut buffer = [0u8; 64 * 1024];
                let mut length = 0u64;
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    length += count as u64;
                    anyhow::ensure!(length <= metadata.len(), "cache changed while validating");
                    hasher.update(&buffer[..count]);
                }
                anyhow::ensure!(length == metadata.len(), "cache changed while validating");
                let digest = format!("{:x}", hasher.finalize());
                entries.push((path.strip_prefix(root)?.to_string_lossy().replace('\\', "/"), metadata.len(), digest));
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries, &mut 0)?;
    entries.sort();
    Ok(entries)
}

fn cached_room_chart(digest: &str) -> Result<Option<String>> {
    anyhow::ensure!(digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()), "invalid chart hash");
    let path = PathBuf::from(dir::charts()?).join("mp-local").join(digest);
    let valid = validate_room_chart_cache(&path, digest).is_ok();
    tracing::info!(cache_hit = valid, "local chart cache checked");
    Ok(valid.then(|| format!("mp-local/{digest}")))
}

fn validate_room_chart_cache(path: &Path, digest: &str) -> Result<()> {
    anyhow::ensure!(!fs::symlink_metadata(path)?.file_type().is_symlink(), "symlink cache root");
    let marker = path.join(CACHE_MANIFEST);
    anyhow::ensure!(fs::metadata(&marker)?.len() <= 1024 * 1024, "invalid cache manifest");
    let (hash, entries): (String, Vec<(String, u64, String)>) = serde_json::from_reader(File::open(marker)?)?;
    anyhow::ensure!(hash == digest && entries == cache_manifest(path)?, "incomplete or modified cache");
    validate_chart_files(path)
}

/// Extract a checked room chart to a content-addressed folder.
/// ZIP size and uncompressed size are both bounded to prevent archive bombs.
fn install_room_chart(bytes: Vec<u8>, digest: String) -> Result<String> {
    let root = PathBuf::from(dir::charts()?).join("mp-local");
    install_room_chart_at(&root, bytes, &digest)?;
    Ok(format!("mp-local/{digest}"))
}

fn install_room_chart_at(root: &Path, bytes: Vec<u8>, digest: &str) -> Result<()> {
    anyhow::ensure!(!bytes.is_empty() && bytes.len() <= phira_mp_common::MAX_LOCAL_CHART_BYTES, "invalid chart ZIP size");
    anyhow::ensure!(format!("{:x}", Sha256::digest(&bytes)) == digest, "chart ZIP checksum mismatch");
    fs::create_dir_all(root)?;
    let target = root.join(digest);
    if validate_room_chart_cache(&target, digest).is_ok() {
        return Ok(());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(&bytes))?;
    if zip.len() > 4096 {
        bail!("local chart archive has too many files");
    }
    let mut extracted = 0u64;
    for i in 0..zip.len() {
        let file = zip.by_index(i)?;
        if file.enclosed_name().is_none() {
            bail!("unsafe path in local chart archive");
        }
        anyhow::ensure!(file.enclosed_name().unwrap().components().count() <= 64, "chart nesting too deep");
        extracted = extracted
            .checked_add(file.size())
            .ok_or_else(|| anyhow!("local chart archive too large"))?;
        if extracted > 128 * 1024 * 1024 {
            bail!("local chart archive expands beyond 128 MiB");
        }
    }
    let strip_root = zip
        .file_names()
        .min_by_key(|name| name.len())
        .filter(|name| name.ends_with('/') && zip.file_names().all(|other| other.starts_with(name)))
        .map(PathBuf::from)
        .unwrap_or_default();
    let tmp = root.join(format!(".partial-{}", Uuid::new_v4()));
    fs::create_dir(&tmp)?;
    let result = (|| -> Result<()> {
        let mut written = 0u64;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let enclosed = entry.enclosed_name().ok_or_else(|| anyhow!("unsafe ZIP path"))?;
            let relative = enclosed.strip_prefix(&strip_root)?;
            if relative.as_os_str().is_empty() {
                continue;
            }
            anyhow::ensure!(relative.components().all(|c| matches!(c, std::path::Component::Normal(_))), "unsafe ZIP path");
            let output = tmp.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(output)?;
            } else {
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent)?;
                }
                let expected = entry.size();
                let mut file = File::create(output)?;
                let count = std::io::copy(&mut (&mut entry).take(128 * 1024 * 1024 - written + 1), &mut file)?;
                written += count;
                anyhow::ensure!(written <= 128 * 1024 * 1024 && count == expected, "invalid extracted chart size");
            }
        }
        validate_chart_files(&tmp)?;
        let manifest = cache_manifest(&tmp)?;
        serde_json::to_writer(File::create(tmp.join(CACHE_MANIFEST))?, &(&digest, manifest))?;
        if target.exists() {
            if validate_room_chart_cache(&target, digest).is_ok() {
                return Ok(());
            }
            fs::remove_dir_all(&target)?;
        }
        if let Err(error) = fs::rename(&tmp, &target) {
            if validate_room_chart_cache(&target, digest).is_err() {
                return Err(error.into());
            }
        }
        Ok(())
    })();
    if result.is_err() || tmp.exists() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

const ENTER_TRANSIT: f32 = 0.5;
const USER_LIST_TRANSIT: f32 = 0.4;
const WIDTH: f32 = 1.6;

const CHAT_ENABLED: bool = cfg!(feature = "chat");

fn screen_size() -> (u32, u32) {
    (screen_width() as u32, screen_height() as u32)
}

struct Message {
    content: String,
    y: f32,
    bottom: f32,
    color: Color,
}

impl Message {
    pub fn text<'a, 's, 'ui>(&'s self, ui: &'ui mut Ui<'a>, mw: f32) -> DrawText<'a, 's, 'ui> {
        ui.text(&self.content)
            .pos(0., self.y)
            .size(0.4)
            .color(self.color)
            .max_width(mw)
            .multiline()
    }
}

type LocalChartTaskResult = (String, u64, std::sync::Weak<Client>);

pub struct MPPanel {
    pub client: Option<Arc<Client>>,

    side_enter_time: f32,

    msg_scroll: Scroll,
    msgs: Vec<Message>,
    msgs_dirty_from: usize,
    last_screen_size: (u32, u32),

    connect_btn: DRectButton,
    connect_task: Option<Task<Result<Client>>>,

    create_room_btn: DRectButton,
    create_room_task: Option<Task<Result<()>>>,
    join_room_btn: DRectButton,
    join_room_task: Option<Task<Result<RoomState>>>,
    leave_room_btn: DRectButton,

    disconnect_btn: DRectButton,

    request_start_btn: DRectButton,
    lock_room_btn: DRectButton,
    cycle_room_btn: DRectButton,

    ready_btn: DRectButton,
    cancel_ready_btn: DRectButton,

    chat_text: String,
    chat_btn: DRectButton,
    chat_send_btn: DRectButton,
    chat_task: Option<Task<Result<()>>>,

    download_task: Option<Task<Result<Arc<Chart>>>>,
    downloading: Option<Downloading>,
    local_chart_task: Option<Task<Result<LocalChartTaskResult>>>,
    local_path: Option<String>,
    local_ready_started: Option<std::time::Instant>,
    // true for request_start, false for ready
    download_next: bool,

    chart_id: Option<i32>,
    game_start_consumed: bool,
    need_upload: bool,
    entered: bool,

    next_scene: Option<NextScene>,

    task: Option<Task<Result<()>>>,

    scene_task: LocalTask<Result<NextScene>>,

    user_list_btn: DRectButton,
    user_list_p: Smooth<f32>,
    user_list_scroll: Scroll,
    icon_user: SafeTexture,
}

impl MPPanel {
    pub fn new(icon_user: SafeTexture) -> Self {
        Self {
            client: None,

            side_enter_time: f32::INFINITY,

            msg_scroll: Scroll::new(),
            msgs: Vec::new(),
            msgs_dirty_from: 0,
            last_screen_size: screen_size(),

            connect_btn: DRectButton::new(),
            connect_task: None,

            create_room_btn: DRectButton::new(),
            create_room_task: None,
            join_room_btn: DRectButton::new(),
            join_room_task: None,
            leave_room_btn: DRectButton::new(),

            disconnect_btn: DRectButton::new(),

            request_start_btn: DRectButton::new(),
            lock_room_btn: DRectButton::new(),
            cycle_room_btn: DRectButton::new(),

            ready_btn: DRectButton::new(),
            cancel_ready_btn: DRectButton::new(),

            chat_text: String::new(),
            chat_btn: DRectButton::new().with_delta(-0.002),
            chat_send_btn: DRectButton::new(),
            chat_task: None,

            download_task: None,
            downloading: None,
            local_chart_task: None,
            local_path: None,
            local_ready_started: None,
            download_next: false,

            chart_id: None,
            game_start_consumed: false,
            need_upload: false,
            entered: false,

            next_scene: None,

            task: None,

            scene_task: None,

            user_list_btn: DRectButton::new(),
            user_list_p: Smooth::default(),
            user_list_scroll: Scroll::new(),
            icon_user,
        }
    }

    fn clone_client(&self) -> Arc<Client> {
        Arc::clone(self.client.as_ref().unwrap())
    }

    fn has_task(&self) -> bool {
        self.connect_task.is_some()
            || self.create_room_task.is_some()
            || self.chat_task.is_some()
            || self.download_task.is_some()
            || self.local_chart_task.is_some()
            || self.task.is_some()
            || self.scene_task.is_some()
    }

    fn connect(&mut self) {
        if get_data().tokens.is_none() {
            show_message(mtl!("connect-must-login")).error();
            return;
        }
        let addr = get_data().config.mp_address.clone();
        self.connect_task = Some(Task::new(async move {
            let token = crate::client::Client::authenticated_token().await?;
            let client = Client::from_address(&addr).await?;
            client
                .authenticate(token)
                .await
                .with_context(|| anyhow!(mtl!("connect-authenticate-failed")))?;
            Ok(client)
        }));
    }

    fn create_room(&mut self, id: RoomId) {
        let client = self.clone_client();
        self.create_room_task = Some(Task::new(async move {
            client.create_room(id).await?;
            Ok(())
        }));
    }

    pub fn select_chart(&mut self, id: i32) {
        let client = self.clone_client();
        if !client.blocking_is_host().unwrap() {
            show_message(mtl!("select-chart-host-only")).error();
            return;
        }
        if !matches!(client.blocking_room_state(), Some(RoomState::SelectChart(_))) {
            show_message(mtl!("select-chart-not-now")).error();
            return;
        }
        self.task = Some(Task::new(async move {
            client.select_chart(id).await.with_context(|| mtl!("select-chart-failed"))?;
            Ok(())
        }));
    }

    /// Room host can select any locally imported chart without publishing it.
    pub fn select_local_chart(&mut self, path: String, name: String) {
        let client = self.clone_client();
        if !client.blocking_is_host().unwrap_or(false) {
            show_message(mtl!("select-chart-host-only")).error();
            return;
        }
        if !matches!(client.blocking_room_state(), Some(RoomState::SelectChart(_))) {
            show_message(mtl!("select-chart-not-now")).error();
            return;
        }
        self.local_path = None;
        self.task = Some(Task::new(async move {
            let source = Path::new(&dir::charts()?).join(path);
            if !source.is_dir() {
                bail!("local chart folder was not found");
            }
            let mut zip = Cursor::new(Vec::new());
            compress_folder(&source, &mut zip)?;
            let bytes = zip.into_inner();
            let digest = format!("{:x}", Sha256::digest(&bytes));
            // Cache the exact uploaded snapshot, not a mutable source directory.
            install_room_chart(bytes.clone(), digest)?;
            client.upload_local_chart(name, bytes).await?;
            Ok(())
        }));
    }

    fn request_start(&mut self) {
        if matches!(self.client.as_ref().unwrap().blocking_room_state().unwrap(), RoomState::SelectChart(None)) {
            show_message(mtl!("request-start-no-chart")).error();
            return;
        }
        self.check_download(true);
    }

    fn check_download(&mut self, next: bool) {
        let id = self.chart_id.unwrap();
        self.download_next = next;
        self.local_ready_started = (id == -1).then(std::time::Instant::now);
        if id == -1 {
            let client = self.clone_client();
            self.local_chart_task = Some(Task::new(async move {
                let epoch = client.chart_epoch();
                let info = client.local_chart_info().await?;
                let path = if let Some(path) = cached_room_chart(&info.sha256)? {
                    path
                } else {
                    let (bytes, digest) = client.download_local_chart_with_info(info).await?;
                    install_room_chart(bytes, digest)?
                };
                anyhow::ensure!(epoch == client.chart_epoch(), "selected chart changed");
                Ok((path, epoch, Arc::downgrade(&client)))
            }));
        } else {
            self.download_task = Some(Task::new(async move { Ptr::new(id).fetch().await }));
        }
    }

    fn post_download(&mut self) {
        let client = self.clone_client();
        let started = self.local_ready_started.take();
        let local_digest = (self.chart_id == Some(-1))
            .then(|| {
                self.local_path
                    .as_ref()
                    .and_then(|path| path.strip_prefix("mp-local/"))
                    .map(str::to_owned)
            })
            .flatten();
        if self.download_next {
            self.task = Some(Task::new(async move {
                if let Some(digest) = local_digest {
                    client.request_start_local_chart(digest).await
                } else {
                    client.request_start().await
                }
                .with_context(|| mtl!("request-start-failed"))?;
                if let Some(started) = started {
                    tracing::info!(seconds = started.elapsed().as_secs_f64(), "host local chart ready");
                }
                Ok(())
            }));
        } else {
            self.task = Some(Task::new(async move {
                if let Some(digest) = local_digest {
                    client.ready_local_chart(digest).await
                } else {
                    client.ready().await
                }
                .with_context(|| mtl!("ready-failed"))?;
                if let Some(started) = started {
                    tracing::info!(seconds = started.elapsed().as_secs_f64(), "local chart player ready");
                }
                Ok(())
            }));
        }
    }
}

impl MPPanel {
    #[inline]
    pub fn in_room(&self) -> bool {
        self.client.as_ref().is_some_and(|it| it.blocking_room_id().is_some())
    }

    #[inline]
    pub fn show(&mut self, rt: f32) {
        self.side_enter_time = rt;
    }

    pub fn enter(&mut self) {
        self.entered = true;
    }

    pub fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> bool {
        let t = tm.now() as f32;
        if self.side_enter_time.is_infinite() {
            return false;
        }
        if self.user_list_p.transiting(t) {
            return true;
        }
        if *self.user_list_p.to() > 0.5 {
            if self.user_list_scroll.touch(touch, t) {
                return true;
            }
            if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.user_list_p.goto(0., t, USER_LIST_TRANSIT);
            }
            return true;
        }
        if !(self.side_enter_time > 0. && tm.real_time() as f32 > self.side_enter_time + ENTER_TRANSIT) {
            return true;
        }
        if self.has_task() {
            return true;
        }
        if let Some(dl) = &mut self.downloading {
            if dl.touch(touch, t) {
                self.downloading = None;
                return true;
            }
        }
        if touch.position.x + 1. > WIDTH {
            self.side_enter_time = -tm.real_time() as f32;
            return true;
        }
        if self.client.is_none() && self.connect_btn.touch(touch, t) {
            self.connect();
            return true;
        }
        if let Some(client) = &self.client {
            if self.msg_scroll.touch(touch, t) {
                return true;
            }
            if let Some(state) = client.blocking_state() {
                if self.chat_btn.touch(touch, t) {
                    request_input("chat", InputBox::new().default_text(&self.chat_text));
                    return true;
                }
                if self.chat_send_btn.touch(touch, t) {
                    if self.chat_text.is_empty() {
                        show_message(mtl!("chat-empty")).error();
                    } else {
                        let client = Arc::clone(client);
                        let text = self.chat_text.clone();
                        self.chat_task = Some(Task::new(async move { client.chat(text).await }));
                    }
                    return true;
                }
                let is_host = state.is_host;
                match state.state {
                    RoomState::SelectChart(_) => {
                        if is_host {
                            if self.request_start_btn.touch(touch, t) {
                                self.request_start();
                                return true;
                            }
                            if self.lock_room_btn.touch(touch, t) {
                                let to = !state.locked;
                                let client = self.clone_client();
                                self.task = Some(Task::new(async move { client.lock_room(to).await.with_context(|| mtl!("lock-room-failed")) }));
                                return true;
                            }
                            if self.cycle_room_btn.touch(touch, t) {
                                let to = !state.cycle;
                                let client = self.clone_client();
                                self.task = Some(Task::new(async move { client.cycle_room(to).await.with_context(|| mtl!("cycle-room-failed")) }));
                                return true;
                            }
                        }
                        if self.leave_room_btn.touch(touch, t) {
                            let client = self.clone_client();
                            self.task = Some(Task::new(async move { client.leave_room().await }));
                            return true;
                        }
                    }
                    RoomState::WaitingForReady => {
                        if client.blocking_is_ready().unwrap() {
                            if self.cancel_ready_btn.touch(touch, t) {
                                let client = self.clone_client();
                                self.task = Some(Task::new(async move { client.cancel_ready().await }));
                                return true;
                            }
                        } else if self.ready_btn.touch(touch, t) {
                            self.check_download(false);
                            return true;
                        }
                    }
                    _ => {}
                }
                if self.user_list_btn.touch(touch, t) {
                    self.user_list_scroll.y_scroller.reset();
                    self.user_list_p.goto(1., t, USER_LIST_TRANSIT);
                    client.blocking_state().unwrap().users.keys().copied().for_each(UserManager::request);
                }
            } else {
                if self.create_room_btn.touch(touch, t) {
                    request_input("room_id", InputBox::new());
                    return true;
                }
                if self.join_room_btn.touch(touch, t) {
                    request_input("join_room", InputBox::new());
                    return true;
                }
                if self.disconnect_btn.touch(touch, t) {
                    self.client = None;
                    self.msgs.clear();
                    self.msgs_dirty_from = 0;
                    return true;
                }
            }
            if client.ping_fail_count() >= 2 && self.connect_task.is_none() {
                warn!("lost connection, reconnecting…");
                show_message(mtl!("reconnect")).warn();
                self.connect();
            }
        }
        true
    }

    pub fn update(&mut self, tm: &mut TimeManager) -> Result<()> {
        let t = tm.now() as f32;
        if self.side_enter_time < 0. && -tm.real_time() as f32 + ENTER_TRANSIT < self.side_enter_time {
            self.side_enter_time = f32::INFINITY;
        }
        let new_size = screen_size();
        if self.last_screen_size != new_size {
            self.last_screen_size = new_size;
            self.msgs_dirty_from = 0;
        }
        self.msg_scroll.update(t);
        if self.user_list_p.now(t) > 1e-4 {
            self.user_list_scroll.update(t);
        }
        if let Some(client) = &self.client {
            self.msgs.extend(client.blocking_take_messages().into_iter().map(|msg| {
                use phira_mp_common::Message as M;
                match msg {
                    M::Chat { user, content, .. } => Message {
                        content: format!("{}：{content}", client.user_name(user)),
                        y: 0.,
                        bottom: 0.,
                        color: WHITE,
                    },
                    msg => {
                        let content = match msg {
                            M::Chat { .. } => unreachable!(),
                            M::CreateRoom { user } => {
                                mtl!("msg-create-room", "user" => client.user_name(user))
                            }
                            M::JoinRoom { name, .. } => {
                                mtl!("msg-join-room", "user" => name)
                            }
                            M::LeaveRoom { name, .. } => {
                                mtl!("msg-leave-room", "user" => name)
                            }
                            M::NewHost { user } => {
                                mtl!("msg-new-host", "user" => client.user_name(user))
                            }
                            M::SelectChart { user, name, id } => {
                                mtl!("msg-select-chart", "user" => client.user_name(user), "chart" => name, "id" => id)
                            }
                            M::GameStart { user } => {
                                mtl!("msg-game-start", "user" => client.user_name(user))
                            }
                            M::Ready { user } => {
                                mtl!("msg-ready", "user" => client.user_name(user))
                            }
                            M::CancelReady { user } => {
                                mtl!("msg-cancel-ready", "user" => client.user_name(user))
                            }
                            M::CancelGame { user } => {
                                mtl!("msg-cancel-game", "user" => client.user_name(user))
                            }
                            M::StartPlaying => mtl!("msg-start-playing").into_owned(),
                            M::Played { user, score, accuracy, full_combo } => {
                                mtl!("msg-played", "user" => client.user_name(user), "score" => format!("{score:07}"), "accuracy" => format!("{:.2}%", accuracy * 100.), "full-combo" => full_combo.to_string())
                            }
                            M::GameEnd => mtl!("msg-game-end").into_owned(),
                            M::Abort { user } => mtl!("msg-abort", "user" => client.user_name(user)),
                            M::LockRoom { lock } => mtl!("msg-room-lock", "lock" => lock.to_string()),
                            M::CycleRoom { cycle } => mtl!("msg-room-cycle", "cycle" => cycle.to_string()),
                            M::SelectLocalChart { user, name } => format!("{} selected local chart: {name}", client.user_name(user)),
                        };
                        Message {
                            content,
                            y: 0.,
                            bottom: 0.,
                            color: semi_white(0.7),
                        }
                    }
                }
            }));
            let state = client.blocking_room_state();
            if matches!(state, Some(RoomState::Playing)) {
                if !self.game_start_consumed {
                    self.game_start_consumed = true;
                    let id = self.chart_id.unwrap();
                    RECORD_ID.store(-1, Ordering::Relaxed);
                    *LOCAL_MP_RESULT.lock().unwrap() = None;
                    self.need_upload = true;
                    self.entered = false;
                    let local_path = if id == -1 {
                        self.local_path.clone().ok_or_else(|| anyhow!("room chart not downloaded"))?
                    } else {
                        format!("download/{id}")
                    };
                    self.scene_task = SongScene::global_launch(
                        (id != -1).then_some(id),
                        &local_path,
                        Mods::default(),
                        GameMode::NoRetry,
                        self.client.as_ref().map(Arc::clone),
                        None,
                        None,
                        false,
                    )?;
                }
            } else {
                self.game_start_consumed = false;
            }
            if let Some(RoomState::SelectChart(chart)) = state {
                self.chart_id = chart;
            }
        }
        if let Some(task) = &mut self.connect_task {
            if let Some(res) = task.take() {
                match res {
                    Ok(client) => {
                        show_message(mtl!("connect-success")).ok();
                        self.client = Some(client.into());
                    }
                    Err(err) => {
                        show_error(err.context(mtl!("connect-failed")));
                    }
                }
                self.connect_task = None;
            }
        }
        if let Some(task) = &mut self.create_room_task {
            if let Some(res) = task.take() {
                match res {
                    Ok(_) => {
                        show_message(mtl!("create-room-success")).ok();
                    }
                    Err(err) => {
                        show_error(err.context(mtl!("create-room-failed")));
                    }
                }
                self.create_room_task = None;
            }
        }
        if let Some(task) = &mut self.download_task {
            if let Some(res) = task.take() {
                match res {
                    Ok(entity) => {
                        let path = format!("download/{}", entity.id);
                        let info_path = format!("{}/{path}/info.yml", dir::charts()?);
                        let should_download = if Path::new(&info_path).exists() {
                            let local_info: ChartInfo = serde_yaml::from_reader(File::open(info_path)?)?;
                            local_info
                                .updated
                                .map_or(entity.updated != entity.created, |local_updated| local_updated != entity.updated)
                        } else {
                            true
                        };
                        if should_download {
                            let info = entity.to_info();
                            self.downloading = Some(SongScene::global_start_download(info, Chart::clone(&entity), {
                                if Path::new(&format!("{}/{path}", dir::charts()?)).exists() {
                                    Some(path)
                                } else {
                                    None
                                }
                            })?);
                        } else {
                            self.post_download();
                        }
                    }
                    Err(err) => {
                        show_error(err.context(mtl!("download-failed")));
                    }
                }
                self.download_task = None;
            }
        }
        if let Some(task) = &mut self.local_chart_task {
            if let Some(res) = task.take() {
                match res {
                    Ok((path, epoch, connection)) => {
                        if self.client.as_ref().is_some_and(|client| {
                            connection.ptr_eq(&Arc::downgrade(client)) && client.chart_epoch() == epoch && client.blocking_room_id().is_some()
                        }) {
                            self.local_path = Some(path);
                            self.post_download();
                        }
                    }
                    Err(err) => show_error(err.context("failed to receive multiplayer local chart")),
                }
                self.local_chart_task = None;
            }
        }
        if let Some(dl) = &mut self.downloading {
            if let Some(res) = dl.check()? {
                if res.is_some() {
                    self.post_download();
                }
                self.downloading = None;
            }
        }
        if let Some(task) = &mut self.chat_task {
            if let Some(res) = task.take() {
                match res {
                    Ok(_) => {
                        show_message(mtl!("chat-sent")).ok();
                        self.chat_text.clear();
                    }
                    Err(err) => {
                        show_error(err.context(mtl!("chat-send-failed")));
                    }
                }
                self.chat_task = None;
            }
        }
        if let Some(task) = &mut self.task {
            if let Some(res) = task.take() {
                if let Err(err) = res {
                    show_error(err);
                }
                self.task = None;
            }
        }
        if let Some(task) = &mut self.join_room_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        show_error(err.context(mtl!("join-room-failed")));
                    }
                    Ok(state) => {
                        self.chart_id = match state {
                            RoomState::SelectChart(id) => id,
                            _ => None,
                        };
                    }
                }
                self.task = None;
            }
        }
        if let Some((id, text)) = take_input() {
            match id.as_str() {
                "chat" => {
                    self.chat_text = text;
                }
                "room_id" => {
                    self.create_room(text.try_into().with_context(|| mtl!("create-invalid-id"))?);
                }
                "join_room" => {
                    let client = self.clone_client();
                    if let Ok(id) = text.try_into() {
                        self.join_room_task = Some(Task::new(async move {
                            client.join_room(id, false).await?;
                            client.room_state().await.ok_or_else(|| anyhow!("expected room state"))
                        }));
                    } else {
                        show_message(mtl!("join-room-invalid-id")).error();
                    }
                }
                _ => return_input(id, text),
            }
        }
        if let Some(task) = &mut self.scene_task {
            if let Some(res) = poll_future(task.as_mut()) {
                match res {
                    Err(err) => {
                        show_error(err);
                    }
                    Ok(scene) => self.next_scene = Some(scene),
                }
                self.scene_task = None;
            }
        }
        if self.need_upload && self.entered {
            let client = self.clone_client();
            if self.chart_id == Some(-1) {
                if let Some(record) = LOCAL_MP_RESULT.lock().unwrap().take() {
                    self.task = Some(Task::new(async move { client.played_local(record.score, record.accuracy, record.full_combo).await }));
                } else {
                    self.task = Some(Task::new(async move { client.abort().await }));
                }
            } else {
                let id = RECORD_ID.load(Ordering::Relaxed);
                if id != -1 {
                    self.task = Some(Task::new(async move { client.played(id).await }));
                } else {
                    self.task = Some(Task::new(async move { client.abort().await }));
                }
            }
            self.need_upload = false;
        }
        Ok(())
    }

    pub fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) {
        let rt = tm.real_time() as f32;
        let t = tm.now() as f32;
        if self.side_enter_time.is_finite() {
            let p = ((rt - self.side_enter_time.abs()) / ENTER_TRANSIT).min(1.);
            let p = 1. - (1. - p).powi(3);
            let p = if self.side_enter_time < 0. { 1. - p } else { p };
            ui.fill_rect(ui.screen_rect(), semi_black(p * 0.6));
            let w = WIDTH;
            let rt = f32::tween(&-1., &(w - 1.), p);
            ui.scope(|ui| {
                ui.dx(rt - w);
                ui.dy(-ui.top);
                let h = ui.top * 2.;
                let r = Rect::new(0., 0., w, h).feather(-0.02);
                ui.fill_path(&r.rounded(0.02), ui.background());
                if let Some(id) = self.client.as_ref().and_then(|it| it.blocking_room_id()) {
                    ui.text(mtl!("room-id", "id" => id.to_string()))
                        .pos(r.right() - 0.02, r.y + 0.02)
                        .anchor(1., 0.)
                        .size(0.44)
                        .color(semi_white(0.4))
                        .draw();
                }
                let tr = ui.text(mtl!("multiplayer")).pos(0.05, 0.05).draw();
                let r = Rect::new(r.x, tr.bottom(), r.w, r.bottom() - tr.bottom()).feather(-0.02);
                if self.client.is_none() {
                    let ct = r.center();
                    self.connect_btn
                        .render_text(ui, Rect::new(ct.x, ct.y, 0., 0.).nonuniform_feather(0.14, 0.06), t, mtl!("connect"), 0.5, true);
                } else {
                    self.render_main(tm, ui, r);
                }
            });
        }
        if let Some(dl) = &mut self.downloading {
            dl.render(ui, t);
        }
        if self.has_task() {
            ui.full_loading_simple(t);
        }
    }

    fn render_main(&mut self, tm: &mut TimeManager, ui: &mut Ui, r: Rect) {
        let t = tm.now() as f32;
        let client = self.client.as_ref().unwrap();
        let mr = Rect::new(r.x, r.y, r.w * 0.8, r.h - if CHAT_ENABLED { 0.11 } else { 0. });
        ui.fill_path(&mr.rounded(0.01), semi_black(0.4));
        ui.scope(|ui| {
            let mut mr = mr.feather(-0.015);
            mr.y -= 0.015;
            mr.h += 0.015;
            ui.dx(mr.x);
            ui.dy(mr.y);
            let mut y = if self.msgs_dirty_from == 0 {
                0.
            } else {
                self.msgs.get(self.msgs_dirty_from - 1).map_or(0., |it| it.bottom)
            };
            let old_dirty = self.msgs_dirty_from != self.msgs.len();
            for msg in &mut self.msgs[self.msgs_dirty_from..] {
                msg.y = y + 0.02;
                msg.bottom = msg.text(ui, mr.w).measure().bottom();
                y = msg.bottom;
            }
            if old_dirty {
                let o = y - mr.h;
                if o >= 0. {
                    self.msg_scroll.y_scroller.goto = Some(o);
                }
            }
            self.msgs_dirty_from = self.msgs.len();
            self.msg_scroll.size((mr.w, mr.h));
            let offset = self.msg_scroll.y_scroller.offset;
            self.msg_scroll.render(ui, |ui| {
                for msg in &self.msgs {
                    if msg.bottom < offset {
                        continue;
                    }
                    if msg.y > offset + mr.h {
                        break;
                    }
                    msg.text(ui, mr.w).draw();
                }
                (mr.w, self.msgs.last().map(|it| it.bottom).unwrap_or_default() + 0.03)
            });
        });

        if CHAT_ENABLED {
            let lw = 0.16;
            let h = 0.09;
            let br = Rect::new(r.x, r.bottom() - h, mr.w - lw - 0.02, h);
            self.chat_btn.render_input(ui, br, t, &self.chat_text, mtl!("chat-placeholder"), 0.5);
            let br = Rect::new(mr.right() - lw, br.y, lw, br.h);
            self.chat_send_btn.render_text(ui, br, t, mtl!("chat-send"), 0.5, true);
        }

        let mut br = Rect::new(mr.right() + 0.02, mr.y, r.right() - mr.right() - 0.02, 0.1);
        let mut btns = SmallVec::<[(&mut DRectButton, String); 5]>::new();
        if let Some(state) = client.blocking_state() {
            match state.state {
                RoomState::SelectChart(_) => {
                    if client.blocking_is_host().unwrap() {
                        btns.push((&mut self.request_start_btn, mtl!("request-start").into_owned()));
                        btns.push((&mut self.lock_room_btn, mtl!("lock-room", "current" => state.locked.to_string())));
                        btns.push((&mut self.cycle_room_btn, mtl!("cycle-room", "current" => state.cycle.to_string())));
                    }
                    btns.push((&mut self.leave_room_btn, mtl!("leave-room").into_owned()));
                }
                RoomState::WaitingForReady => {
                    if client.blocking_is_ready().unwrap() {
                        btns.push((&mut self.cancel_ready_btn, mtl!("cancel-ready").into_owned()));
                    } else {
                        btns.push((&mut self.ready_btn, mtl!("ready").into_owned()));
                    }
                }
                _ => {}
            }
            btns.push((&mut self.user_list_btn, mtl!("user-list").into_owned()));
        } else {
            btns.push((&mut self.create_room_btn, mtl!("create-room").into_owned()));
            btns.push((&mut self.join_room_btn, mtl!("join-room").into_owned()));
            btns.push((&mut self.disconnect_btn, mtl!("disconnect").into_owned()));
        }
        for (btn, text) in btns {
            btn.render_text(ui, br, t, text, 0.5, true);
            br.y += br.h + 0.02;
        }

        let p = self.user_list_p.now(t);
        if p > 1e-4 {
            ui.abs_scope(|ui| {
                ui.alpha(p, |ui| {
                    let users: Vec<_> = client.blocking_state().unwrap().users.values().cloned().collect();
                    let n = users.len();
                    let columns = n.clamp(2, 4);
                    let rn = n.div_ceil(columns);
                    ui.fill_rect(ui.screen_rect(), semi_black(p * 0.4));

                    let mut iter = users.into_iter();
                    let h = 0.14;
                    let w = 0.48;
                    let pad = 0.03;
                    let width = w * columns as f32 + pad * (columns - 1) as f32;
                    let viewport_height = (ui.top * 2. - 0.16).max(h);
                    ui.dx(-width / 2.);
                    ui.dy(-ui.top + 0.08);
                    self.user_list_scroll.size((width, viewport_height));
                    self.user_list_scroll.render(ui, |ui| {
                        for i in 0..rn {
                            let cn = (n - i * columns).min(columns);
                            let row_width = w * cn as f32 + pad * (cn - 1) as f32;
                            let row_offset = (width - row_width) / 2.;
                            for j in 0..cn {
                                let r = Rect::new(row_offset + j as f32 * (w + pad), i as f32 * (h + pad), w, h);
                                let Some(user) = iter.next() else { unreachable!() };
                                ui.avatar(r.x + 0.055, r.center().y, 0.04, t, UserManager::opt_avatar(user.id, &self.icon_user));
                                ui.text(user.name)
                                    .pos(r.x + 0.105, r.center().y)
                                    .anchor(0., 0.5)
                                    .no_baseline()
                                    .max_width(0.36)
                                    .size(0.55)
                                    .draw();
                            }
                        }
                        (width, (rn as f32 * (h + pad) - pad).max(0.))
                    });
                });
            });
        }
    }

    #[inline]
    pub fn next_scene(&mut self) -> Option<NextScene> {
        self.next_scene.take()
    }
}

#[cfg(test)]
mod room_cache_tests {
    use super::*;
    use std::io::Write;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn atomic_install_reuses_and_repairs_validated_cache() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = archive(&[
            ("info.yml", b"chart: chart.json\nmusic: song.mp3\nillustration: background.png\n"),
            ("chart.json", b"{}"),
            ("song.mp3", b"music"),
            ("background.png", b"image"),
        ]);
        let digest = format!("{:x}", Sha256::digest(&bytes));
        install_room_chart_at(dir.path(), bytes.clone(), &digest).unwrap();
        validate_room_chart_cache(&dir.path().join(&digest), &digest).unwrap();
        install_room_chart_at(dir.path(), bytes.clone(), &digest).unwrap();
        fs::write(dir.path().join(&digest).join("chart.json"), b"modified").unwrap();
        install_room_chart_at(dir.path(), bytes, &digest).unwrap();
        validate_room_chart_cache(&dir.path().join(&digest), &digest).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn unsafe_or_incomplete_archives_never_become_cache_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        for bytes in [archive(&[("../outside", b"bad")]), archive(&[("chart.json", b"{}")])] {
            let digest = format!("{:x}", Sha256::digest(&bytes));
            assert!(install_room_chart_at(&root, bytes, &digest).is_err());
            assert!(!root.join(digest).exists());
            assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        }
        assert!(!dir.path().join("outside").exists());
    }

    #[test]
    fn only_complete_unchanged_installations_are_cache_hits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let digest = "a".repeat(64);
        assert!(validate_room_chart_cache(root, &digest).is_err());
        fs::write(root.join("info.yml"), "chart: chart.json\nmusic: song.mp3\nillustration: background.png\n").unwrap();
        for name in ["chart.json", "song.mp3", "background.png"] {
            fs::write(root.join(name), b"test").unwrap();
        }
        // An arbitrary chart directory is still not a completed installation.
        assert!(validate_room_chart_cache(root, &digest).is_err());
        let manifest = cache_manifest(root).unwrap();
        serde_json::to_writer(File::create(root.join(CACHE_MANIFEST)).unwrap(), &(&digest, manifest)).unwrap();
        validate_room_chart_cache(root, &digest).unwrap();
        validate_room_chart_cache(root, &digest).unwrap();
        assert!(validate_room_chart_cache(root, &"b".repeat(64)).is_err());
        fs::write(root.join("chart.json"), b"edit").unwrap();
        assert!(validate_room_chart_cache(root, &digest).is_err());
        fs::write(root.join("chart.json"), b"test").unwrap();
        fs::remove_file(root.join("song.mp3")).unwrap();
        assert!(validate_room_chart_cache(root, &digest).is_err());
    }

    #[test]
    fn chart_references_cannot_escape_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("info.yml"), "chart: ../outside.json\n").unwrap();
        assert!(validate_chart_files(dir.path()).is_err());
    }
}
