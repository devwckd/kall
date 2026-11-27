//! Files in chat, on the client: uploads waiting to go out with the next message, downloads
//! (to the Downloads folder, or to a cache to view or play), image previews, the players for
//! inline video and audio, and the full-size image viewer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kall_proto::Id;
use kall_proto::files::{Attachment, FileKind, human_size};

use crate::State;
use crate::net::NetCmd;

/// A file attached to the message being written.
pub struct Pending {
    pub job: u64,
    pub name: String,
    pub size: u64,
    pub done: u64,
    pub state: Up,
}

pub enum Up {
    Sending,
    Ready(Attachment),
    Failed(String),
}

/// A download in progress or finished.
pub struct Dl {
    pub done: u64,
    pub total: u64,
    pub state: DlState,
}

pub enum DlState {
    Running,
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone, Copy)]
enum Job {
    Upload,
    /// (file, to the Downloads folder rather than the cache)
    Download(Id, bool),
}

#[derive(Default)]
pub struct Files {
    next_job: u64,
    jobs: HashMap<u64, Job>,
    pub pending: Vec<Pending>,
    /// Downloads to the cache, for viewing and playing.
    pub cache: HashMap<Id, Dl>,
    /// Downloads to the Downloads folder.
    pub saved: HashMap<Id, Dl>,
    pub thumbs: HashMap<Id, rune::Pixels>,
    thumbs_asked: HashSet<Id>,
    pub players: HashMap<Id, rune::Player>,
    /// Play once the cache download finishes.
    play_when_ready: HashSet<Id>,
    /// The image open full size, and its decoded pictures (filled from a worker thread).
    pub viewer: Option<Attachment>,
    pub full: Arc<Mutex<HashMap<Id, rune::Pixels>>>,
    /// Files are being dragged over the chat.
    pub drop_hover: bool,
}

/// Where viewed and played files are kept.
fn cache_path(att: &Attachment) -> PathBuf {
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("kall").join("files").join(format!("{}-{}", att.id, att.name))
}

/// A free name in the Downloads folder: "photo.png", then "photo (1).png", …
fn download_path(name: &str) -> PathBuf {
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

pub fn open_path(path: &Path) -> Result<(), String> {
    opener::open(path).map_err(|e| e.to_string())
}

pub fn reveal_path(path: &Path) -> Result<(), String> {
    opener::reveal(path).map_err(|e| e.to_string())
}

impl State {
    fn job(&mut self, j: Job) -> u64 {
        self.files.next_job += 1;
        let id = self.files.next_job;
        self.files.jobs.insert(id, j);
        id
    }

    /// Start uploading files for the next message.
    pub fn attach_paths(&mut self, paths: &[PathBuf]) {
        let max = self.m.as_ref().map_or(u64::MAX, |m| m.limits.max_bytes);
        for path in paths {
            let Ok(meta) = std::fs::metadata(path) else { continue };
            if !meta.is_file() {
                continue;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            if meta.len() > max {
                self.toast(format!("{name} is too big: files can be up to {}.", human_size(max)));
                continue;
            }
            if self.files.pending.len() >= 10 {
                self.toast("Up to 10 files per message.");
                break;
            }
            let job = self.job(Job::Upload);
            self.cmd(NetCmd::Upload { job, path: path.clone() });
            self.files.pending.push(Pending { job, name, size: meta.len(), done: 0, state: Up::Sending });
        }
    }

    /// A pasted image: save it as a PNG and attach it.
    pub fn attach_png(&mut self, png: &[u8]) {
        let dir = std::env::temp_dir().join("kall").join("pasted");
        let path = dir.join(format!("pasted-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, png)) {
            Ok(()) => self.attach_paths(&[path]),
            Err(e) => self.toast(format!("Couldn't paste the image: {e}")),
        }
    }

    /// Take a file off the message being written (cancelling its upload).
    pub fn remove_pending(&mut self, job: u64) {
        self.cmd(NetCmd::Cancel(job));
        self.files.jobs.remove(&job);
        self.files.pending.retain(|p| p.job != job);
    }

    /// Still uploading something for the next message.
    pub fn uploads_busy(&self) -> bool {
        self.files.pending.iter().any(|p| matches!(p.state, Up::Sending))
    }

    /// The uploaded files to send with the next message (and clear them).
    pub fn take_attachments(&mut self) -> Vec<Id> {
        let ids = self.files.pending.iter().filter_map(|p| if let Up::Ready(a) = &p.state { Some(a.id) } else { None }).collect();
        self.files.pending.clear();
        ids
    }

    /// Ask for the previews of images in the open channel's loaded history.
    pub fn want_visible_thumbs(&mut self) {
        let Some(log) = self.m.as_ref().and_then(|m| m.logs.get(&self.channel)) else { return };
        let want: Vec<Id> = log.msgs.iter().rev().take(150).flat_map(|m| &m.attachments).filter(|a| a.thumb && !a.expired && !self.files.thumbs_asked.contains(&a.id)).map(|a| a.id).collect();
        for f in want {
            self.want_thumb(f);
        }
    }

    pub fn want_thumb(&mut self, file: Id) {
        if self.files.thumbs_asked.insert(file) {
            self.cmd(NetCmd::FetchThumb { file });
        }
    }

    fn download(&mut self, att: &Attachment, save: bool) {
        let job = self.job(Job::Download(att.id, save));
        let dest = if save { download_path(&att.name) } else { cache_path(att) };
        let map = if save { &mut self.files.saved } else { &mut self.files.cache };
        map.insert(att.id, Dl { done: 0, total: att.size, state: DlState::Running });
        self.cmd(NetCmd::Download { job, file: att.id, dest });
    }

    /// Save a file to the Downloads folder.
    pub fn save_file(&mut self, att: &Attachment) {
        if !matches!(self.files.saved.get(&att.id).map(|d| &d.state), Some(DlState::Running)) {
            self.download(att, true);
        }
    }

    /// Stop downloading `file` (either kind).
    pub fn cancel_download(&mut self, file: Id) {
        let jobs: Vec<u64> = self.files.jobs.iter().filter(|(_, j)| matches!(j, Job::Download(f, _) if *f == file)).map(|(k, _)| *k).collect();
        for job in jobs {
            self.cmd(NetCmd::Cancel(job));
            self.files.jobs.remove(&job);
        }
        self.files.cache.remove(&file);
        self.files.saved.remove(&file);
        self.files.play_when_ready.remove(&file);
    }

    /// Show an image full size (downloading it first).
    pub fn view_image(&mut self, att: &Attachment) {
        self.files.viewer = Some(att.clone());
        match self.files.cache.get(&att.id).map(|d| &d.state) {
            Some(DlState::Done(path)) => self.decode_full(att.id, path.clone()),
            Some(DlState::Running) => {}
            _ => {
                let cached = cache_path(att);
                if cached.exists() {
                    self.files.cache.insert(att.id, Dl { done: att.size, total: att.size, state: DlState::Done(cached.clone()) });
                    self.decode_full(att.id, cached);
                } else {
                    self.download(att, false);
                }
            }
        }
    }

    fn decode_full(&self, file: Id, path: PathBuf) {
        if self.files.full.lock().unwrap().contains_key(&file) {
            return;
        }
        let (full, wake) = (self.files.full.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let Ok(img) = image::open(&path) else { return };
            // Big photos are shown scaled; the file on disk keeps every pixel.
            let img = if img.width().max(img.height()) > 2560 { img.resize(2560, 2560, image::imageops::FilterType::Triangle) } else { img };
            let rgba = img.to_rgba8();
            let px = rune::Pixels::new();
            px.set(rgba.width(), rgba.height(), rgba.into_raw());
            full.lock().unwrap().insert(file, px);
            if let Some(w) = wake {
                w();
            }
        });
    }

    /// Play a video or audio file inline (downloading it first), or pause/resume it.
    pub fn toggle_play(&mut self, att: &Attachment) {
        if let Some(p) = self.files.players.get(&att.id) {
            if p.state() == rune::PlayerState::Playing {
                p.pause();
            } else {
                p.play();
            }
            return;
        }
        match self.files.cache.get(&att.id).map(|d| &d.state) {
            Some(DlState::Done(path)) => {
                let path = path.clone();
                self.start_player(att.id, att.kind, path);
            }
            Some(DlState::Running) => {}
            _ => {
                let cached = cache_path(att);
                if cached.exists() {
                    self.files.cache.insert(att.id, Dl { done: att.size, total: att.size, state: DlState::Done(cached.clone()) });
                    self.start_player(att.id, att.kind, cached);
                } else {
                    self.files.play_when_ready.insert(att.id);
                    self.download(att, false);
                }
            }
        }
    }

    fn start_player(&mut self, file: Id, kind: FileKind, path: PathBuf) {
        let p = if kind == FileKind::Audio { rune::Player::audio(path) } else { rune::Player::file(path) };
        p.set_muted(false);
        p.play();
        self.files.players.insert(file, p);
    }

    /// Something is playing (the UI redraws a few times a second for its progress).
    pub fn playing(&self) -> bool {
        self.files.players.values().any(|p| p.state() == rune::PlayerState::Playing)
    }

    pub fn on_transfer_progress(&mut self, job: u64, done: u64, total: u64) {
        match self.files.jobs.get(&job).copied() {
            Some(Job::Upload) => {
                if let Some(p) = self.files.pending.iter_mut().find(|p| p.job == job) {
                    p.done = done;
                }
            }
            Some(Job::Download(file, save)) => {
                let map = if save { &mut self.files.saved } else { &mut self.files.cache };
                if let Some(d) = map.get_mut(&file) {
                    (d.done, d.total) = (done, total);
                }
            }
            None => {}
        }
    }

    pub fn on_uploaded(&mut self, job: u64, att: Attachment) {
        self.files.jobs.remove(&job);
        if let Some(p) = self.files.pending.iter_mut().find(|p| p.job == job) {
            p.done = p.size;
            p.state = Up::Ready(att);
        }
    }

    pub fn on_downloaded(&mut self, job: u64, path: PathBuf) {
        let Some(Job::Download(file, save)) = self.files.jobs.remove(&job) else { return };
        let map = if save { &mut self.files.saved } else { &mut self.files.cache };
        if let Some(d) = map.get_mut(&file) {
            d.done = d.total;
            d.state = DlState::Done(path.clone());
        }
        if save {
            return;
        }
        if self.files.viewer.as_ref().is_some_and(|v| v.id == file) {
            self.decode_full(file, path.clone());
        }
        if self.files.play_when_ready.remove(&file) {
            let kind = kall_proto::files::FileKind::of(&path.to_string_lossy());
            self.start_player(file, kind, path);
        }
    }

    pub fn on_transfer_failed(&mut self, job: u64, error: String) {
        match self.files.jobs.remove(&job) {
            Some(Job::Upload) => {
                if let Some(p) = self.files.pending.iter_mut().find(|p| p.job == job) {
                    p.state = Up::Failed(error);
                }
            }
            Some(Job::Download(file, save)) => {
                let map = if save { &mut self.files.saved } else { &mut self.files.cache };
                if let Some(d) = map.get_mut(&file) {
                    d.state = DlState::Failed(error.clone());
                }
                self.files.play_when_ready.remove(&file);
                self.toast(error);
            }
            None => {}
        }
    }

    pub fn on_thumb(&mut self, file: Id, w: u32, h: u32, rgba: Vec<u8>) {
        let px = rune::Pixels::new();
        px.set(w, h, rgba);
        self.files.thumbs.insert(file, px);
    }

    /// The connection dropped: transfers in flight are gone.
    pub fn transfers_lost(&mut self) {
        for (job, j) in std::mem::take(&mut self.files.jobs) {
            match j {
                Job::Upload => {
                    if let Some(p) = self.files.pending.iter_mut().find(|p| p.job == job) {
                        p.state = Up::Failed("Connection lost".into());
                    }
                }
                Job::Download(file, save) => {
                    let map = if save { &mut self.files.saved } else { &mut self.files.cache };
                    map.remove(&file);
                }
            }
        }
        self.files.thumbs_asked.retain(|f| self.files.thumbs.contains_key(f));
        self.files.play_when_ready.clear();
    }
}
