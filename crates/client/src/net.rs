//! The network thread: one QUIC connection to the server, reconnects, the saved session in the
//! OS keychain, and pinned certificates. The UI talks to it through [`Net`]: commands in,
//! events out (each event wakes the UI, which drains them in `poll`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use kall_proto::{C2S, PROTOCOL_VERSION, S2C};
use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::voice::VoicePrefs;
use crate::voice::engine::VoiceLink;

pub enum Auth {
    Login { username: String, password: String },
    Register { invite: String, username: String, password: String },
    Resume,
}

pub enum NetCmd {
    Connect { server: String, auth: Auth },
    Send(C2S),
    UploadAvatar(PathBuf),
    /// Forget the pinned certificate for a server (after it was legitimately replaced).
    ForgetPin(String),
    Logout,
}

pub enum NetEvent {
    /// Saved login details, for the login form.
    Prefill { server: String, username: String },
    Connecting { server: String, attempt: u32 },
    Msg(S2C),
    Avatar { user: kall_proto::Id, rev: u32, size: u32, rgba: Vec<u8> },
    /// Back to the login screen, with why.
    LoggedOut { reason: Option<String>, pin_mismatch: bool },
    Disconnected { retry_in: u64 },
    Notice(String),
}

pub struct Net {
    pub tx: UnboundedSender<NetCmd>,
    pub events: mpsc::Receiver<NetEvent>,
    /// Voice datagrams in and out, shared with the voice engine.
    pub voice: Arc<VoiceLink>,
}

impl Net {
    pub fn send(&self, cmd: NetCmd) {
        self.tx.send(cmd).ok();
    }
    pub fn c2s(&self, msg: C2S) {
        self.send(NetCmd::Send(msg));
    }
}

/// `KALL_PROFILE=name` keeps a separate saved login (handy for two clients on one machine).
fn profile() -> String {
    std::env::var("KALL_PROFILE").ok().filter(|p| !p.is_empty()).map_or(String::new(), |p| format!("-{p}"))
}

fn app_id() -> String {
    format!("dev.kall.client{}", profile())
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Saved {
    server: String,
    username: String,
    /// server address → pinned certificate fingerprint
    pins: BTreeMap<String, String>,
}

fn load_saved() -> Saved {
    let prefs = rune::prefs::Prefs::open(&app_id());
    prefs.get("saved").unwrap_or_default()
}

fn store_saved(s: &Saved) {
    let mut prefs = rune::prefs::Prefs::open(&app_id());
    prefs.set("saved", s).ok();
}

pub fn load_app_prefs() -> crate::desktop::AppPrefs {
    rune::prefs::Prefs::open(&app_id()).get("app").unwrap_or_default()
}

pub fn save_app_prefs(p: crate::desktop::AppPrefs) {
    std::thread::spawn(move || {
        rune::prefs::Prefs::open(&app_id()).set("app", &p).ok();
    });
}

pub fn load_voice_prefs() -> VoicePrefs {
    rune::prefs::Prefs::open(&app_id()).get("voice").unwrap_or_default()
}

/// Saved on a short-lived thread: no file I/O on the UI thread.
pub fn save_voice_prefs(p: VoicePrefs) {
    std::thread::spawn(move || {
        rune::prefs::Prefs::open(&app_id()).set("voice", &p).ok();
    });
}

fn keychain(server: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(&format!("kall{}", profile()), &server.to_lowercase()).ok()
}

fn load_token(server: &str) -> Option<String> {
    keychain(server)?.get_password().ok()
}

fn save_token(server: &str, token: &str) {
    if let Some(e) = keychain(server)
        && let Err(err) = e.set_password(token)
    {
        eprintln!("kall: can't save the session in the keychain: {err}");
    }
}

fn forget_token(server: &str) {
    if let Some(e) = keychain(server) {
        e.delete_credential().ok();
    }
}

/// Start the network thread. `wake` asks the UI for a frame.
pub fn start(wake: rune::Waker) -> Net {
    let (tx, rx) = unbounded_channel();
    let (etx, events) = mpsc::channel();
    let voice = Arc::new(VoiceLink::default());
    let link = voice.clone();
    std::thread::Builder::new()
        .name("kall-net".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
            rt.block_on(run(rx, Events { tx: etx, wake }, link));
        })
        .expect("net thread");
    Net { tx, events, voice }
}

struct Events {
    tx: mpsc::Sender<NetEvent>,
    wake: rune::Waker,
}

impl Events {
    fn emit(&self, e: NetEvent) {
        self.tx.send(e).ok();
        (self.wake)();
    }
}

enum End {
    /// Connection dropped after a good login: try again.
    Lost,
    /// Don't retry; back to the login screen.
    LoggedOut(Option<String>, bool),
    /// A new Connect arrived while we were connected.
    Switch(String, Auth),
}

async fn run(mut rx: UnboundedReceiver<NetCmd>, ev: Events, link: Arc<VoiceLink>) {
    let mut saved = load_saved();
    ev.emit(NetEvent::Prefill { server: saved.server.clone(), username: saved.username.clone() });
    let mut next: Option<(String, Auth)> = None;
    if !saved.server.is_empty() && load_token(&saved.server).is_some() {
        next = Some((saved.server.clone(), Auth::Resume));
    }
    loop {
        let (server, auth) = match next.take() {
            Some(n) => n,
            None => match rx.recv().await {
                Some(NetCmd::Connect { server, auth }) => (server, auth),
                Some(NetCmd::ForgetPin(server)) => {
                    saved.pins.remove(&server.to_lowercase());
                    store_saved(&saved);
                    ev.emit(NetEvent::Notice("Forgot the pinned certificate. Connect again to trust the new one.".into()));
                    continue;
                }
                Some(_) => continue,
                None => return,
            },
        };
        let mut auth = Some(auth);
        let mut attempt = 0u32;
        let mut established = false;
        loop {
            ev.emit(NetEvent::Connecting { server: server.clone(), attempt });
            let a = auth.take().unwrap_or(Auth::Resume);
            let r = session(&server, a, &mut saved, &mut rx, &ev, &mut established, &link).await;
            *link.conn.lock().unwrap() = None;
            match r {
                Ok(End::Lost) | Err(_) if established => {
                    attempt += 1;
                    let wait = [1, 2, 3, 5, 10][(attempt as usize - 1).min(4)];
                    ev.emit(NetEvent::Disconnected { retry_in: wait });
                    // Wait, but stay responsive to a logout or a new login meanwhile.
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                        cmd = rx.recv() => match cmd {
                            Some(NetCmd::Logout) => {
                                forget_token(&server);
                                ev.emit(NetEvent::LoggedOut { reason: None, pin_mismatch: false });
                                break;
                            }
                            Some(NetCmd::Connect { server, auth }) => { next = Some((server, auth)); break; }
                            None => return,
                            _ => {}
                        }
                    }
                }
                Ok(End::Switch(s, a)) => {
                    next = Some((s, a));
                    break;
                }
                Ok(End::LoggedOut(reason, pin)) => {
                    ev.emit(NetEvent::LoggedOut { reason, pin_mismatch: pin });
                    break;
                }
                Ok(End::Lost) => {
                    ev.emit(NetEvent::LoggedOut { reason: Some("Lost the connection.".into()), pin_mismatch: false });
                    break;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let pin = msg.contains("certificate changed");
                    ev.emit(NetEvent::LoggedOut { reason: Some(msg), pin_mismatch: pin });
                    break;
                }
            }
        }
    }
}

async fn session(
    server: &str,
    auth: Auth,
    saved: &mut Saved,
    rx: &mut UnboundedReceiver<NetCmd>,
    ev: &Events,
    established: &mut bool,
    link: &Arc<VoiceLink>,
) -> Result<End> {
    let key = server.trim().to_lowercase();
    let first = match auth {
        Auth::Login { username, password } => C2S::Login { version: PROTOCOL_VERSION, username, password },
        Auth::Register { invite, username, password } => C2S::Register { version: PROTOCOL_VERSION, invite, username, password },
        Auth::Resume => match load_token(&key) {
            Some(token) => C2S::Resume { version: PROTOCOL_VERSION, token },
            None => return Ok(End::LoggedOut(None, false)),
        },
    };
    let resuming = matches!(first, C2S::Resume { .. });
    let pin = saved.pins.get(&key).cloned();
    let mut c = tokio::time::timeout(Duration::from_secs(8), kall_proto::client::connect(server, pin.clone()))
        .await
        .map_err(|_| anyhow!("Couldn't reach {server} (timed out)."))??;
    kall_proto::send(&mut c.send, &first).await?;
    let username = match tokio::time::timeout(Duration::from_secs(15), kall_proto::recv::<S2C>(&mut c.recv)).await.map_err(|_| anyhow!("The server didn't answer."))?? {
        Some(S2C::AuthOk { token, username }) => {
            save_token(&key, &token);
            username
        }
        Some(S2C::AuthError { reason }) => {
            if resuming {
                forget_token(&key);
            }
            return Ok(End::LoggedOut(Some(reason), false));
        }
        _ => return Err(anyhow!("The server sent something unexpected.")),
    };
    // Trust on first use: pin a self-signed certificate we just talked to. CA-verified ones
    // renew every few months, so they're never pinned.
    if c.ca_verified {
        saved.pins.remove(&key);
    } else if pin.is_none() {
        saved.pins.insert(key.clone(), c.fingerprint.clone());
    }
    saved.server = server.trim().to_string();
    saved.username = username;
    store_saved(saved);
    *established = true;

    // Voice datagrams bypass the UI: straight to the voice engine, when one is running.
    *link.conn.lock().unwrap() = Some(c.conn.clone());
    let (dconn, dlink) = (c.conn.clone(), link.clone());
    let datagrams = tokio::spawn(async move {
        while let Ok(d) = dconn.read_datagram().await {
            let route = if d.first() == Some(&kall_proto::video::KIND_VIDEO) { &dlink.video_inbound } else { &dlink.inbound };
            if let Some(tx) = route.lock().unwrap().as_ref() {
                tx.send(d.to_vec()).ok();
            }
        }
    });
    let (mut send, mut recv) = (c.send, c.recv);
    let (in_tx, mut in_rx) = unbounded_channel::<Result<Option<S2C>>>();
    let reader = tokio::spawn(async move {
        loop {
            let r = kall_proto::recv::<S2C>(&mut recv).await;
            let done = !matches!(r, Ok(Some(_)));
            if in_tx.send(r).is_err() || done {
                break;
            }
        }
    });
    let end = loop {
        tokio::select! {
            msg = in_rx.recv() => match msg {
                Some(Ok(Some(S2C::Avatar { user, rev, png }))) => {
                    let tx = ev.tx.clone();
                    let wake = ev.wake.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Ok(img) = image::load_from_memory(&png) {
                            let img = img.resize_to_fill(128, 128, image::imageops::FilterType::Triangle).to_rgba8();
                            tx.send(NetEvent::Avatar { user, rev, size: 128, rgba: img.into_raw() }).ok();
                            wake();
                        }
                    });
                }
                Some(Ok(Some(S2C::Removed { reason }))) => {
                    forget_token(&key);
                    break End::LoggedOut(Some(reason), false);
                }
                Some(Ok(Some(m))) => ev.emit(NetEvent::Msg(m)),
                Some(Ok(None)) | Some(Err(_)) | None => break End::Lost,
            },
            cmd = rx.recv() => match cmd {
                Some(NetCmd::Send(m)) => {
                    if kall_proto::send(&mut send, &m).await.is_err() {
                        break End::Lost;
                    }
                }
                Some(NetCmd::UploadAvatar(path)) => {
                    match tokio::task::spawn_blocking(move || avatar_png(&path)).await? {
                        Ok(png) => { kall_proto::send(&mut send, &C2S::SetAvatar { png }).await.ok(); }
                        Err(e) => ev.emit(NetEvent::Notice(format!("Couldn't use that image: {e}"))),
                    }
                }
                Some(NetCmd::Logout) => {
                    kall_proto::send(&mut send, &C2S::Logout).await.ok();
                    forget_token(&key);
                    break End::LoggedOut(None, false);
                }
                Some(NetCmd::ForgetPin(_)) => {}
                Some(NetCmd::Connect { server, auth }) => break End::Switch(server, auth),
                None => break End::LoggedOut(None, false),
            },
        }
    };
    reader.abort();
    datagrams.abort();
    c.conn.close(0u32.into(), b"bye");
    Ok(end)
}

/// Any image → a 128×128 centre-cropped PNG.
fn avatar_png(path: &PathBuf) -> Result<Vec<u8>> {
    let img = image::open(path)?;
    let img = img.resize_to_fill(128, 128, image::imageops::FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}
