# Changelog

Each release's section here is also the top of its GitHub release notes.

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
