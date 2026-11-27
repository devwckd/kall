# Changelog

Each release's section here is also the top of its GitHub release notes.

## Unreleased

### Fixed
- **Sound cues on Linux** (joining, mute, pings) play through PulseAudio or PipeWire instead of ALSA's compatibility layer, so they're no longer choppy. They also play on devices that don't use 32-bit float audio. (#36)
- **Shorter device lists on Linux.** The microphone and speaker lists show your sound server's devices, the same ones your system's sound settings show, without ALSA's aliases or monitor sources. (#37)
- **AppImage updates.** kall restarts into the new AppImage instead of the old one. When it can't replace its file (no write permission, or not run as an AppImage), the update card says why and has a **Download** button. The README has Linux install tips. (#38)

## v0.3.2

### New
- **Screen share on AMD and Intel GPUs on Linux** (VAAPI), alongside NVIDIA. The Linux packages now need libva, which desktop distros already have. (#30)
- **GPU video decoding** (D3D11VA, VideoToolbox, VAAPI), off by default and turned on with `KALL_GPU_DECODE=1`. Copying frames back from the GPU made it slower than decoding on the CPU. (#30)

## v0.3.1

### New
- **Screen share on the GPU on macOS (VideoToolbox) and Linux (NVIDIA NVENC)**, like Windows got in 0.3.0. (#26)

### Changed
- While watching a stream, the sharer's tile turns into **Stop watching**, and the separate button above the stage is gone. (#24)
- The "Voice activity: just talk" pill is gone. Push-to-talk users see their key in the mic's right-click panel. (#25)

## v0.3.0

### New
- **Crop your avatar.** After picking an image, drag and zoom it in a round frame before saving. (#7, #21)
- **Share files in chat.**
  - Attach with the paperclip, paste a screenshot, or drag files onto the chat. Files show progress while uploading.
  - Images show inline, with a full-size viewer.
  - Video and audio play right in the chat (Windows and macOS).
  - Other files get a card with Download, Open and Show in folder.
  - Files can be up to 100 MB and are kept for 30 days. The server owner can change both with `KALL_MAX_FILE_MB` and `KALL_FILE_DAYS`. (#12)
- **Screen share on the GPU (Windows).** Sharing your screen or camera encodes on the graphics card (NVIDIA NVENC, AMD AMF or Intel Quick Sync) through FFmpeg, with the CPU as fallback. Watching uses FFmpeg's faster decoder. Much lighter while gaming. (#11)

### Upgrading
- **Update the server and the apps together.** Messages with attachments changed the protocol, so 0.2 apps are asked to update when they connect.

## v0.2.0

### New
- **Screen shares are easy to spot.** The sharer's tile shows a live preview with a **Watch stream** button. Everyone in the call gets a toast with a **Watch** button and a sound. The sidebar shows a red **LIVE** pill (click it to watch) and a camera mark for cameras. (#1, #2)
- **New share picker.** It has Screens and Windows tabs, a grid of live thumbnails, and clearer quality and sound options. (#2)
- **Window sound only.** Sharing a window now shares only that app's sound. Sharing a whole screen still shares everything except kall. (#2)
- **Quick audio panel.** Right-click the mute or deafen button to switch your microphone or speakers, change the volume, or toggle mute/deafen. (#3, #9)
- **Mic volume and output volume** settings, in the quick panel and in Settings → Voice & audio. (#9)
- Settings → App shows the app's version.

### Fixed
- **Avatar upload.** "Change avatar" opens the file picker again, and the new avatar is applied when you press Save. (#4, #8)

### Upgrading
- **Update the server too** (`docker compose pull && docker compose up -d`). Stream previews come from the new server. Older apps keep working with it, but they don't show previews.

## v0.1.2
- The ghost icon on the taskbar and on `kall.exe`.

## v0.1.1
- No more console window next to kall on Windows.

## v0.1.0
- First release: text chat, voice, screen share and webcam, invites and roles, home hosting with Let's Encrypt and DDNS, installers and signed auto-updates.
