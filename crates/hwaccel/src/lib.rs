//! FFmpeg's hardware video APIs, wrapped for kall: GPU decoding (D3D11VA on Windows,
//! VideoToolbox on macOS, VAAPI on Linux) and VAAPI encoding (AMD and Intel GPUs on Linux).
//!
//! `ffmpeg-next` has no safe wrappers for hardware devices and frames, so this small crate
//! holds the FFI (`unsafe`) and the app (which forbids `unsafe`) only sees safe functions.

use std::ptr;

use ffmpeg_next as ff;
use ff::ffi;

/// The GPU decoding API for this platform, and the pixel format its frames come in.
fn platform_decode() -> Option<(ffi::AVHWDeviceType, ffi::AVPixelFormat, &'static str)> {
    if cfg!(windows) {
        Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA, ffi::AVPixelFormat::AV_PIX_FMT_D3D11, "D3D11VA"))
    } else if cfg!(target_os = "macos") {
        Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX, ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX, "VideoToolbox"))
    } else if cfg!(target_os = "linux") {
        Some((ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI, ffi::AVPixelFormat::AV_PIX_FMT_VAAPI, "VAAPI"))
    } else {
        None
    }
}

/// A new hardware device of `kind`, or None when the GPU or driver doesn't offer one.
fn device(kind: ffi::AVHWDeviceType) -> Option<*mut ffi::AVBufferRef> {
    let mut dev: *mut ffi::AVBufferRef = ptr::null_mut();
    // SAFETY: av_hwdevice_ctx_create writes a new reference to `dev` on success (0).
    let r = unsafe { ffi::av_hwdevice_ctx_create(&mut dev, kind, ptr::null(), ptr::null_mut(), 0) };
    (r >= 0 && !dev.is_null()).then_some(dev)
}

/// The decoder's get_format callback: take the hardware format when offered, else the first
/// (software) one, so a stream the GPU can't decode still decodes on the CPU.
unsafe extern "C" fn pick_format(_ctx: *mut ffi::AVCodecContext, mut fmts: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    let Some((_, hw, _)) = platform_decode() else { return unsafe { *fmts } };
    let first = unsafe { *fmts };
    // SAFETY: FFmpeg passes a list ended by AV_PIX_FMT_NONE.
    unsafe {
        while *fmts != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *fmts == hw {
                return hw;
            }
            fmts = fmts.add(1);
        }
    }
    first
}

/// Make a decoder (not yet opened) decode on the GPU. Returns the API's name, or None when
/// there's no GPU decoding here (the decoder stays on the CPU).
pub fn attach_decoder(ctx: &mut ff::codec::context::Context) -> Option<&'static str> {
    let (kind, _, name) = platform_decode()?;
    let dev = device(kind)?;
    // SAFETY: the context is open for configuration (not opened yet); it takes over our
    // reference to the device and frees it with the context.
    unsafe {
        let c = ctx.as_mut_ptr();
        (*c).hw_device_ctx = dev;
        (*c).get_format = Some(pick_format);
    }
    Some(name)
}

/// A decoded frame in GPU memory, copied into normal memory (NV12 usually); None when it was
/// already a software frame.
pub fn download(frame: &ff::frame::Video) -> Result<Option<ff::frame::Video>, ff::Error> {
    // SAFETY: reading the format and hw_frames_ctx of a valid frame.
    let on_gpu = unsafe { !(*frame.as_ptr()).hw_frames_ctx.is_null() };
    if !on_gpu {
        return Ok(None);
    }
    let mut sw = ff::frame::Video::empty();
    // SAFETY: both frames are valid; FFmpeg allocates `sw`'s buffers in a format the GPU
    // frames can be copied to.
    let r = unsafe { ffi::av_hwframe_transfer_data(sw.as_mut_ptr(), frame.as_ptr(), 0) };
    if r < 0 {
        return Err(ff::Error::from(r));
    }
    Ok(Some(sw))
}

/// A pool of VAAPI frames for an encoder: upload NV12 frames, send the uploaded ones.
pub struct VaapiFrames {
    frames: *mut ffi::AVBufferRef,
}

// SAFETY: the pool is reference-counted by FFmpeg and only used from the encoder's thread.
unsafe impl Send for VaapiFrames {}

impl Drop for VaapiFrames {
    fn drop(&mut self) {
        // SAFETY: we own one reference.
        unsafe { ffi::av_buffer_unref(&mut self.frames) };
    }
}

impl VaapiFrames {
    /// A VAAPI device and a pool of `width`×`height` NV12 surfaces; None without VAAPI.
    pub fn new(width: u32, height: u32) -> Option<VaapiFrames> {
        let mut dev = device(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)?;
        // SAFETY: allocating and configuring a frames context on a live device, then
        // dropping our device reference (the frames context keeps its own).
        unsafe {
            let mut frames = ffi::av_hwframe_ctx_alloc(dev);
            ffi::av_buffer_unref(&mut dev);
            if frames.is_null() {
                return None;
            }
            let fc = (*frames).data as *mut ffi::AVHWFramesContext;
            (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*fc).sw_format = ffi::AVPixelFormat::AV_PIX_FMT_NV12;
            (*fc).width = width as i32;
            (*fc).height = height as i32;
            (*fc).initial_pool_size = 8;
            if ffi::av_hwframe_ctx_init(frames) < 0 {
                ffi::av_buffer_unref(&mut frames);
                return None;
            }
            Some(VaapiFrames { frames })
        }
    }

    /// Give an encoder (not yet opened, format VAAPI) these frames.
    pub fn attach_encoder(&self, enc: &mut ff::codec::context::Context) {
        // SAFETY: the encoder isn't open; it gets its own reference.
        unsafe { (*enc.as_mut_ptr()).hw_frames_ctx = ffi::av_buffer_ref(self.frames) };
    }

    /// Copy an NV12 frame to a GPU surface, ready to send to the encoder.
    pub fn upload(&self, nv12: &ff::frame::Video) -> Result<ff::frame::Video, ff::Error> {
        let mut hw = ff::frame::Video::empty();
        // SAFETY: getting a surface from our pool, then copying the NV12 planes into it.
        unsafe {
            let r = ffi::av_hwframe_get_buffer(self.frames, hw.as_mut_ptr(), 0);
            if r < 0 {
                return Err(ff::Error::from(r));
            }
            let r = ffi::av_hwframe_transfer_data(hw.as_mut_ptr(), nv12.as_ptr(), 0);
            if r < 0 {
                return Err(ff::Error::from(r));
            }
        }
        hw.set_pts(nv12.pts());
        hw.set_kind(nv12.kind());
        Ok(hw)
    }
}
