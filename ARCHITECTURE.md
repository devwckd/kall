# kall architecture

kall is built for one group of friends (about ten people) and one server. The client has no
server list, no federation and no end-to-end encryption: everything is encrypted in transit by
QUIC's TLS 1.3, and the server is trusted.

## Workspace

```text
crates/proto    wire types (serde + postcard), framing, media packet formats, QUIC connect helper
crates/server   kall-server: quinn endpoint, SQLite, voice/video SFU, ACME, DDNS, link previews
crates/client   kall: Rune UI, network thread, voice and video pipelines, desktop integration
```

## One QUIC connection per client

Everything uses one UDP port (default `7744`), with the ALPN `kall/1`.

- **Control stream** (bidirectional, opened first): length-prefixed postcard messages (`C2S` /
  `S2C` in `crates/proto/src/lib.rs`). It carries auth, chat, presence, moderation, and
  voice/video signalling.
- **Extra streams**, one per file transfer: an upload or a download of a file shared in chat
  (`crates/proto/src/files.rs`), so a big file never holds up the control stream. Closing the
  stream cancels the transfer.
- **Datagrams** (RFC 9221): media, sent without retransmission.
  - **Voice:** one 20 ms Opus frame (48 kHz mono) per datagram, `[kind][seq][flags][opus]`.
    The server prefixes the speaker's id and forwards the datagram without decoding it.
  - **Video:** an encoded frame split into 1100-byte fragments, `[kind][frame][index][count][flags][data]`.
    A receiver that loses part of a frame asks for a keyframe on the control stream, and the
    server passes the request on to the sender.

The network thread (tokio + quinn) hands events to the UI through a Rune `Waker`, and the UI
handles them in `poll`.

## Server

- tokio + quinn. SQLite through `rusqlite`, with calls kept off the async threads.
- **Data:** users, sessions, invites, channels (kind, position, bitrate), messages (reply,
  edited, deleted), read state, bans and link previews. History is kept forever.
- **Accounts:** registration needs an invite code. Passwords are hashed with argon2. A session
  token, kept in the client's OS keychain, logs back in.
- **Roles:** owner, admin, mod and member, each allowed a fixed set of actions (kick, ban,
  delete others' messages, manage channels, create invites).
- **Bootstrap:** on first run, with no users, the server prints an owner invite.
  `kall-server invite` mints more.
- **Certificates:**
  - With a domain and a Cloudflare token, the server gets a Let's Encrypt certificate over
    ACME DNS-01 (`instant-acme`), renews it and swaps it in live.
  - Without them, it makes a self-signed certificate. Clients reaching a self-signed server by
    IP pin it on first connect (trust on first use); a server reached by domain must present a
    CA-trusted certificate.
- **DDNS:** the server checks the public IP periodically and updates the A (and optionally
  AAAA) record with the same Cloudflare token.
- **SFU:**
  - Voice goes to everyone else in the speaker's channel.
  - Video goes only to the people watching that stream.
  - A share's system audio goes only to the people watching that share.
- **Link previews:** the server fetches the page and reads its title, description and image.
  It refuses addresses in private, loopback and link-local ranges, so a link can't be used to
  probe the server's LAN.

## Client

- **UI:** [Rune](https://github.com/anomalyco/rune), pinned to one revision in the workspace
  `Cargo.toml`. Views live in `crates/client/src/ui/`. The look is flat, with edge-to-edge
  panes, one emerald accent, and dark and light palettes that follow the OS.
- **Voice** (`crates/client/src/voice/`):
  - **Devices:** `cpal` for audio I/O.
  - **Microphone processing:** a high-pass filter, echo cancellation (AEC3, via `aec3`) and gain
    control.
  - **Noise suppression and voice activity:** RNNoise (`nnnoiseless`). Voice activity is gated
    with a 60 ms pre-roll and a 400 ms hangover, so the start and end of words aren't clipped.
  - **Push-to-talk:** works globally, even while a game has focus.
  - **Encoding:** Opus, with in-band FEC.
  - **Playback:** each speaker gets a jitter buffer with loss concealment, then everything is
    mixed with per-user gain.
- **Video** (`crates/client/src/video/`):
  - **Capture:**
    - Windows: Windows.Graphics.Capture (`windows-capture`).
    - macOS and Linux: `xcap`.
    - Webcam: `nokhwa`.
  - **System audio:** WASAPI process loopback on Windows, which excludes kall's own output so
    friends don't hear themselves.
  - **Codec:** H.264 through OpenH264, on the CPU. It encodes sliced and multithreaded, at
    about 8 ms per 1080p frame on a modern desktop CPU.
- **Desktop:** OS notifications (`rune-notify`), a tray icon with an unread badge, close to
  tray, optional start at login (`auto-launch`) and sound cues.
- **Updates:** official builds read `latest.json` from the GitHub release. They install the
  new version only if its minisign signature matches `crates/client/updater.pub`.

## Ideas not built yet

- Hardware video encoding (NVENC, AMF, QSV, VideoToolbox, VAAPI) and AV1.
- Direct messages and file uploads in chat.
- Native capture APIs on macOS (ScreenCaptureKit) and Linux (PipeWire portal), and system
  audio there.
- Code signing and notarization for the installers.
