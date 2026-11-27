//! Screen share and camera in a call: what we send, what we receive, and which streams we
//! watch. Lives beside the voice engine and starts and stops with it.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, channel};

use kall_proto::{C2S, Id, StreamInfo, StreamKind};

use crate::State;
use crate::net::NetCmd;
use crate::video::capture::{self, Capture, Source};
use crate::video::recv::Incoming;
use crate::video::send::Outgoing;
use crate::video::{CAMERA_QUALITY, QUALITIES, camera, share_audio};

pub struct Sharing {
    pub out: Outgoing,
    _capture: Capture,
    _audio: Option<share_audio::Running>,
    pub info: StreamInfo,
}

pub struct Cam {
    pub out: Outgoing,
    _cam: camera::Running,
}

pub struct Picker {
    pub sources: Option<Vec<Source>>,
    pub selected: Option<String>,
    pub quality: usize,
    pub audio: bool,
}

#[derive(Default)]
pub struct Media {
    pub rx: Option<Incoming>,
    pub sharing: Option<Sharing>,
    pub camera: Option<Cam>,
    camera_starting: Option<(Outgoing, Receiver<anyhow::Result<camera::Running>>)>,
    share_starting: Option<(Outgoing, StreamInfo, Receiver<anyhow::Result<(Capture, Option<share_audio::Running>, Option<String>)>>)>,
    /// Streams we asked the server for.
    watching: HashSet<(Id, StreamKind)>,
    /// The screen share shown big on the stage.
    pub focus: Option<Id>,
    pub picker: Option<Picker>,
    sources_rx: Option<Receiver<Vec<Source>>>,
    /// Bumped by the video threads on every new frame, so the UI knows to redraw.
    seen: u64,
}

impl State {
    pub(crate) fn start_media(&mut self) {
        let Some(net) = &self.net else { return };
        let (tx, rx) = channel();
        *net.voice.video_inbound.lock().unwrap() = Some(tx);
        let cmd = net.tx.clone();
        let wake = self.waker.clone().expect("waker");
        self.media.rx = Some(Incoming::start(rx, move |m| drop(cmd.send(NetCmd::Send(m))), wake));
    }

    pub(crate) fn stop_media(&mut self) {
        if let Some(net) = &self.net {
            *net.voice.video_inbound.lock().unwrap() = None;
        }
        self.media = Media::default();
    }

    /// The server says someone needs a keyframe of our stream.
    pub(crate) fn own_keyframe(&mut self, kind: StreamKind) {
        match kind {
            StreamKind::Screen => self.media.sharing.as_ref().map(|s| s.out.request_keyframe()),
            StreamKind::Camera => self.media.camera.as_ref().map(|c| c.out.request_keyframe()),
        };
    }

    // ---- screen share ----

    pub fn open_share_picker(&mut self) {
        let (tx, rx) = channel();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            tx.send(capture::list()).ok();
            if let Some(w) = wake {
                w();
            }
        });
        self.media.sources_rx = Some(rx);
        self.media.picker = Some(Picker { sources: None, selected: None, quality: 1, audio: share_audio::supported() });
    }

    pub fn start_share(&mut self) {
        let (Some(net), Some(p)) = (&self.net, &self.media.picker) else { return };
        let Some(source) = p.sources.as_ref().and_then(|ss| ss.iter().find(|s| Some(&s.id) == p.selected.as_ref())).cloned() else { return };
        let q = QUALITIES[p.quality.min(QUALITIES.len() - 1)];
        let wake = self.waker.clone().expect("waker");
        let out = Outgoing::start(StreamKind::Screen, q, net.voice.clone(), wake.clone());
        let (slot, link, want_audio) = (out.slot.clone(), net.voice.clone(), p.audio);
        let info = StreamInfo { kind: StreamKind::Screen, width: q.max_w as u32, height: q.max_h as u32, fps: q.fps, audio: want_audio, label: source.name.clone() };
        let (tx, rx) = channel();
        // Starting capture and system audio can take a moment: off the UI thread.
        std::thread::spawn(move || {
            let r = capture::start(&source, q.fps, slot).map_err(|e| anyhow::anyhow!("Couldn't capture {}: {e:#}", source.name)).map(|cap| {
                let (audio, warn) = if want_audio {
                    match share_audio::start(link) {
                        Ok(a) => (Some(a), None),
                        Err(e) => (None, Some(format!("Sharing without sound: {e:#}"))),
                    }
                } else {
                    (None, None)
                };
                (cap, audio, warn)
            });
            tx.send(r).ok();
            wake();
        });
        self.media.share_starting = Some((out, info, rx));
        self.media.picker = None;
    }

    pub fn stop_share(&mut self) {
        self.media.share_starting = None;
        if self.media.sharing.take().is_some() {
            self.send(C2S::StopStream { kind: StreamKind::Screen });
        }
        if self.media.focus == self.m.as_ref().map(|m| m.me) {
            self.media.focus = None;
        }
    }

    // ---- camera ----

    pub fn toggle_camera(&mut self) {
        if self.media.camera.take().is_some() || self.media.camera_starting.take().is_some() {
            self.send(C2S::StopStream { kind: StreamKind::Camera });
            return;
        }
        let Some(net) = &self.net else { return };
        let wake = self.waker.clone().expect("waker");
        let out = Outgoing::start(StreamKind::Camera, CAMERA_QUALITY, net.voice.clone(), wake.clone());
        let slot = out.slot.clone();
        let index = self.voice.camera;
        let (tx, rx) = channel();
        // Opening a camera can take a second or two: off the UI thread.
        std::thread::spawn(move || {
            let index = index.or_else(|| camera::list().first().map(|c| c.index));
            let r = match index {
                Some(i) => camera::start(i, slot),
                None => Err(anyhow::anyhow!("No camera found.")),
            };
            tx.send(r).ok();
            wake();
        });
        self.media.camera_starting = Some((out, rx));
    }

    /// Watch a screen share big on the stage (or stop, with `None`).
    pub fn focus_stream(&mut self, user: Option<Id>) {
        self.media.focus = user;
    }

    /// Keep the server's idea of what we watch in step with what's on screen, and pick up
    /// background results. Returns true when the UI should redraw.
    pub(crate) fn sync_media(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.media.sources_rx
            && let Ok(list) = rx.try_recv()
        {
            if let Some(p) = &mut self.media.picker {
                p.selected = list.first().map(|s| s.id.clone());
                p.sources = Some(list);
            }
            self.media.sources_rx = None;
            changed = true;
        }
        if let Some((_, rx)) = &self.media.camera_starting
            && let Ok(r) = rx.try_recv()
        {
            let (out, _) = self.media.camera_starting.take().unwrap();
            match r {
                Ok(cam) => {
                    let q = CAMERA_QUALITY;
                    self.send(C2S::StartStream(StreamInfo { kind: StreamKind::Camera, width: q.max_w as u32, height: q.max_h as u32, fps: q.fps, audio: false, label: "Camera".into() }));
                    self.media.camera = Some(Cam { out, _cam: cam });
                }
                Err(e) => self.toast(format!("{e:#}")),
            }
            changed = true;
        }
        if let Some((_, _, rx)) = &self.media.share_starting
            && let Ok(r) = rx.try_recv()
        {
            let (out, mut info, _) = self.media.share_starting.take().unwrap();
            match r {
                Ok((cap, audio, warn)) => {
                    if let Some(w) = warn {
                        self.toast(w);
                    }
                    info.audio = audio.is_some();
                    self.send(C2S::StartStream(info.clone()));
                    self.media.sharing = Some(Sharing { out, _capture: cap, _audio: audio, info });
                    self.media.focus = self.m.as_ref().map(|m| m.me);
                }
                Err(e) => self.toast(format!("{e:#}")),
            }
            changed = true;
        }
        let Some(m) = &self.m else { return changed };
        let Some(mine) = self.in_voice_here(None).map(|v| v.channel) else { return changed };
        // Cameras of everyone in the call, and the focused screen share.
        let mut want: HashSet<(Id, StreamKind)> = HashSet::new();
        for (&(user, kind), _) in m.streams.iter() {
            let here = m.voice.get(&user).is_some_and(|v| v.channel == mine);
            if user == m.me || !here {
                continue;
            }
            if kind == StreamKind::Camera || (kind == StreamKind::Screen && self.media.focus == Some(user)) {
                want.insert((user, kind));
            }
        }
        // Our own share counts from the moment it starts, before the server echoes it back.
        let own = |u: Id| u == m.me && self.media.sharing.is_some();
        let focus_gone = self.media.focus.is_some_and(|u| !own(u) && !m.streams.contains_key(&(u, StreamKind::Screen)));
        let start: Vec<_> = want.difference(&self.media.watching).copied().collect();
        let stop: Vec<_> = self.media.watching.difference(&want).copied().collect();
        for (user, kind) in start {
            self.send(C2S::WatchStream { user, kind, watch: true });
        }
        for (user, kind) in stop {
            self.send(C2S::WatchStream { user, kind, watch: false });
        }
        self.media.watching = want;
        if focus_gone {
            self.media.focus = None;
            changed = true;
        }
        // New frames since the last look?
        let frames = self.media.rx.as_ref().map_or(0, |r| r.frames_seen())
            + self.media.sharing.as_ref().map_or(0, |s| s.out.previews())
            + self.media.camera.as_ref().map_or(0, |c| c.out.previews());
        if frames != self.media.seen {
            self.media.seen = frames;
            changed = true;
        }
        changed
    }

    /// The latest picture of `user`'s stream: ours from the local preview, others' decoded.
    pub fn stream_pixels(&self, user: Id, kind: StreamKind) -> Option<rune::Pixels> {
        let me = self.m.as_ref().map(|m| m.me);
        if Some(user) == me {
            return match kind {
                StreamKind::Screen => self.media.sharing.as_ref().map(|s| s.out.preview.clone()),
                StreamKind::Camera => self.media.camera.as_ref().map(|c| c.out.preview.clone()),
            };
        }
        self.media.rx.as_ref()?.frames.lock().unwrap().get(&(user, kind)).cloned()
    }
}
