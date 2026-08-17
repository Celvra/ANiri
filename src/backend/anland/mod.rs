//! Anland backend: renders niri directly into the Android consumer's dmabufs.
//!
//! The anland transport is a buffer-sharing protocol: the Android consumer owns a
//! set of dmabufs (plus a "buffer ready" eventfd, a data socket and a fence
//! socket). We render niri's scene into the consumer-selected dmabuf with EGL,
//! hand over a native fence and `trigger_refresh()`, then wait for the consumer's
//! buffer-ready signal before completing the frame.

pub mod ffi;
pub mod input;

use std::mem;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use niri_config::OutputName;
use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::egl::native::EGLSurfacelessDisplay;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::damage::{OutputDamageTracker, RenderOutputResult};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, Color32F, ImportDma};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::Size;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal};
use smithay::wayland::presentation::Refresh;

use super::{IpcOutputMap, OutputId, RenderResult};
use crate::niri::{Niri, RedrawState};
use crate::render_helpers::{resources, shaders, RenderCtx, RenderTarget};
use crate::utils::{get_monotonic_time, logical_output};

/// How often we poll the consumer for input, buffer-ready and reconnects.
/// Kept at 1 ms so buffer-ready (our "vblank") is picked up with minimal
/// latency; the anland lockstep throughput is bound by this plus the render.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Converts the anland protocol pixel format into a DRM fourcc.
fn protocol_format_to_fourcc(format: u32) -> Fourcc {
    // Same as kwin's anland backend: 1 == DRM_FORMAT_ABGR8888, else XRGB8888.
    match format {
        1 => Fourcc::Abgr8888,
        _ => Fourcc::Xrgb8888,
    }
}

fn egl_device_for_env() -> Option<EGLDevice> {
    let env = std::env::var_os("ANLAND_DRM_DEVICE")
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty());

    if let Ok(devices) = EGLDevice::enumerate() {
        for dev in devices {
            let matches = if let Some(ref desired) = env {
                let drm_matches = dev
                    .drm_device_path()
                    .map(|p| p.to_string_lossy().into_owned() == *desired)
                    .unwrap_or(false);
                let render_matches = dev
                    .render_device_path()
                    .map(|p| p.to_string_lossy().into_owned() == *desired)
                    .unwrap_or(false);
                drm_matches || render_matches
            } else {
                true
            };
            if matches {
                return Some(dev);
            }
        }
    }

    None
}

fn create_egl_display() -> anyhow::Result<EGLDisplay> {
    if let Some(device) = egl_device_for_env() {
        match unsafe { EGLDisplay::new(device) } {
            Ok(display) => {
                info!("anland: created EGL display from a device");
                return Ok(display);
            }
            Err(err) => {
                warn!("anland: error creating EGL display from device, falling back: {err:?}");
            }
        }
    }
    let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }
        .context("error creating EGL display (surfaceless)")?;
    Ok(display)
}

pub struct Anland {
    ctx: *mut ffi::display_ctx,
    screen_w: u32,
    screen_h: u32,
    screen_refresh: u32,
    renderer: Option<GlesRenderer>,
    damage_tracker: Option<OutputDamageTracker>,
    dmabuf_global: Option<DmabufGlobal>,
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,
    output: Option<Output>,
    input_backend: input::AnlandInputBackend,
    /// Whether the consumer has presented the last frame (or just connected) and
    /// is ready for another one. Gates rendering so we never draw into a buffer
    /// the consumer is still displaying.
    consumer_ready: bool,
    pending_feedback: Option<OutputPresentationFeedback>,
    frame_seq: u64,
    frame_stats_count: u32,
    frame_stats_start: Duration,
    /// The frame number in which each consumer dmabuf was last rendered into.
    /// Used to compute the buffer `age` for the damage tracker. Passing age 0
    /// forces a full-output redraw every frame, which re-reads every client
    /// buffer; software (SHM/llvmpipe) clients rewrite their released buffers
    /// and that mid-write read shows up as random flicker. A real age makes the
    /// damage tracker redraw only what changed.
    buffer_last_rendered_frame: [u64; ffi::MAX_BUFS],
    /// The `Dmabuf` we render into per consumer buffer index. smithay's GL
    /// renderer caches the imported EGL image / texture / renderbuffer keyed by
    /// the `Dmabuf` object; if we built a fresh `Dmabuf` every frame that cache
    /// never hits, so each frame created and destroyed new EGL images, and on
    /// freedreno/KGSL the teardown freed the BO while the GPU still referenced
    /// it ("premature free" GPU page faults → flicker / corruption). Keeping the
    /// same `Dmabuf` alive makes the renderer reuse the cached resources.
    consumer_dmabufs: [Option<Dmabuf>; ffi::MAX_BUFS],
    /// Signalled by the selection handler when a client sets the clipboard; the
    /// poll timer reads the new selection and forwards it to the consumer.
    selection_tx: std::sync::mpsc::Sender<()>,
    selection_rx: std::sync::mpsc::Receiver<()>,
    /// Signalled by the clipboard reader thread with the compositor clipboard text.
    clipboard_tx: std::sync::mpsc::Sender<String>,
    clipboard_rx: std::sync::mpsc::Receiver<String>,
    disposed: bool,
}

unsafe impl Send for Anland {}

impl Anland {
    pub fn new() -> anyhow::Result<Self> {
        // NOTE: we used to unset GALLIUM_DRIVER=kgsl here because the stock Arch
        // mesa (26.1.x) couldn't create a surfaceless DRI screen with the KGSL
        // backend, forcing the compositor onto llvmpipe. With the
        // lfdevs/mesa-for-android-container build (26.2.0+, "fix KGSL
        // initialization for surfaceless and Wayland") the KGSL backend works,
        // and /etc/environment already exports GALLIUM_DRIVER/MESA_LOADER_
        // DRIVER_OVERRIDE=kgsl + FD_FORCE_KGSL=1 for native Adreno GL. Do NOT
        // touch them here.

        let socket_path =
            std::env::var("ANLAND_SOCKET").unwrap_or_else(|_| "/run/display.sock".to_string());
        let path =
            std::ffi::CString::new(socket_path.as_str()).context("invalid ANLAND_SOCKET path")?;

        let mut ctx = std::ptr::null_mut();
        let r = unsafe { ffi::connect_to_deamon(&mut ctx, path.as_ptr()) };
        if r != 0 || ctx.is_null() {
            anyhow::bail!("anland: error connecting to daemon at {socket_path}");
        }

        let mut width = 0u32;
        let mut height = 0u32;
        let mut format = 0u32;
        let mut refresh = 0u32;
        if unsafe { ffi::get_screen_info(ctx, &mut width, &mut height, &mut format, &mut refresh) }
            < 0
        {
            unsafe { ffi::disconnect(ctx) };
            anyhow::bail!("anland: error getting screen info");
        }

        info!("anland: connected to daemon, screen {width}x{height} fmt {format} refresh {refresh} mHz");

        let ipc_outputs = Arc::new(Mutex::new(IpcOutputMap::new()));
        let input_backend = input::AnlandInputBackend {
            native_w: width,
            native_h: height,
            time_offset: 0,
        };
        let (selection_tx, selection_rx) = std::sync::mpsc::channel();
        let (clipboard_tx, clipboard_rx) = std::sync::mpsc::channel();

        Ok(Self {
            ctx,
            screen_w: width,
            screen_h: height,
            screen_refresh: refresh,
            renderer: None,
            damage_tracker: None,
            dmabuf_global: None,
            ipc_outputs,
            output: None,
            input_backend,
            consumer_ready: false,
            pending_feedback: None,
            frame_seq: 0,
            frame_stats_count: 0,
            frame_stats_start: Duration::ZERO,
            buffer_last_rendered_frame: [u64::MAX; ffi::MAX_BUFS],
            consumer_dmabufs: [const { None }; ffi::MAX_BUFS],
            selection_tx,
            selection_rx,
            clipboard_tx,
            clipboard_rx,
            disposed: false,
        })
    }

    /// Sender for the selection handler to request a clipboard forward.
    pub fn clipboard_selection_tx(&self) -> std::sync::mpsc::Sender<()> {
        self.selection_tx.clone()
    }

    pub fn init(&mut self, niri: &mut Niri) {
        if let Err(err) = self.create_renderer() {
            error!("anland: error initializing renderer: {err:#}");
            return;
        }

        niri.update_shaders();

        self.create_dmabuf_global(niri);
        self.add_output(niri);

        #[cfg(have_anland_audio)]
        if unsafe { ffi::anland_audio_start() } != 0 {
            warn!("anland: failed to start audio engine");
        }
        #[cfg(have_anland_audio)]
        if unsafe { ffi::anland_camera_start() } != 0 {
            warn!("anland: failed to start camera engine");
        }

        let timer = Timer::from_duration(POLL_INTERVAL);
        niri.event_loop
            .insert_source(timer, move |_, _, state| {
                // Reconnect and buffer-ready handling.
                state.backend.anland().poll(&mut state.niri);

                // Drain pending input events from the consumer.
                while let Some(event) = state.backend.anland().poll_input(&mut state.niri) {
                    state.process_input_event(event);
                }

                state.refresh_and_flush_clients();
                TimeoutAction::ToDuration(POLL_INTERVAL)
            })
            .expect("failed to insert anland poll timer");
    }

    /// Advertises the DRM render node to wayland clients via the dmabuf
    /// feedback global, so that EGL clients (zink) can create a hardware
    /// DRI2 screen instead of falling back to llvmpipe.
    fn create_dmabuf_global(&mut self, niri: &mut Niri) {
        let Some(renderer) = self.renderer.as_mut() else {
            warn!("anland: no renderer, skipping dmabuf global");
            return;
        };

        let default_feedback = || {
            let display = renderer.egl_context().display();
            let device =
                EGLDevice::device_for_display(display).context("error getting EGL device")?;
            let node = device
                .try_get_render_node()
                .context("error getting EGL device render node")?
                .context("failed to query EGL device render node")?;

            let primary_formats = renderer.dmabuf_formats();
            DmabufFeedbackBuilder::new(node.dev_id(), primary_formats)
                .build()
                .context("error building dmabuf feedback")
        };

        let dmabuf_global = match default_feedback() {
            Ok(feedback) => niri
                .dmabuf_state
                .create_global_with_default_feedback::<crate::niri::State>(
                    &niri.display_handle,
                    &feedback,
                ),
            Err(err) => {
                warn!(
                    "anland: failed building default dmabuf feedback, falling back to v3: {err:?}"
                );
                let primary_formats = renderer.dmabuf_formats();
                niri.dmabuf_state
                    .create_global::<crate::niri::State>(&niri.display_handle, primary_formats)
            }
        };
        assert!(self.dmabuf_global.replace(dmabuf_global).is_none());
    }

    fn create_renderer(&mut self) -> anyhow::Result<()> {
        let display = create_egl_display()?;
        let context = EGLContext::new(&display).context("error creating EGL context")?;
        let mut renderer =
            unsafe { GlesRenderer::new(context) }.context("error creating GLES renderer")?;

        resources::init(&mut renderer);
        shaders::init(&mut renderer);

        self.renderer = Some(renderer);
        Ok(())
    }

    fn add_output(&mut self, niri: &mut Niri) {
        let connector = "anland-0".to_string();
        let make = "niri".to_string();
        let model = "anland".to_string();
        let serial = "0".to_string();

        let output = Output::new(
            connector.clone(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: make.clone(),
                model: model.clone(),
                serial_number: serial.clone(),
            },
        );

        let mode = Mode {
            size: Size::from((self.screen_w as i32, self.screen_h as i32)),
            refresh: self.screen_refresh as i32,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);

        output.user_data().insert_if_missing(|| OutputName {
            connector,
            make: Some(make),
            model: Some(model),
            serial: Some(serial),
        });

        let physical_properties = output.physical_properties();
        self.ipc_outputs.lock().unwrap().insert(
            OutputId::next(),
            niri_ipc::Output {
                name: output.name(),
                make: physical_properties.make,
                model: physical_properties.model,
                serial: None,
                physical_size: None,
                modes: vec![niri_ipc::Mode {
                    width: self.screen_w as u16,
                    height: self.screen_h as u16,
                    refresh_rate: self.screen_refresh,
                    is_preferred: true,
                }],
                current_mode: Some(0),
                is_custom_mode: true,
                vrr_supported: false,
                vrr_enabled: false,
                logical: Some(logical_output(&output)),
                max_bpc: None,
            },
        );

        self.output = Some(output.clone());
        self.damage_tracker = Some(OutputDamageTracker::from_output(&output));
        niri.add_output(output, None, false);
    }

    /// The consumer switched resolution/orientation. Adopt the new buffer size as
    /// our output mode so rendering resumes instead of skipping every frame.
    fn adapt_to_size(&mut self, niri: &mut Niri, output: &Output, w: i32, h: i32) {
        info!("anland: consumer changed size to {w}x{h}, adapting output");
        self.screen_w = w as u32;
        self.screen_h = h as u32;
        self.input_backend.native_w = self.screen_w;
        self.input_backend.native_h = self.screen_h;

        let mode = Mode {
            size: Size::from((w, h)),
            refresh: self.screen_refresh as i32,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);

        {
            let mut ipc_outputs = self.ipc_outputs.lock().unwrap();
            if let Some(output) = ipc_outputs.values_mut().next() {
                let mode = &mut output.modes[0];
                mode.width = w.clamp(0, u16::MAX as i32) as u16;
                mode.height = h.clamp(0, u16::MAX as i32) as u16;
                if let Some(logical) = output.logical.as_mut() {
                    logical.width = w as u32;
                    logical.height = h as u32;
                }
            }
            niri.ipc_outputs_changed = true;
        }

        niri.output_resized(output);
    }

    pub fn seat_name(&self) -> String {
        "anland".to_owned()
    }

    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        self.renderer.as_mut().map(f)
    }

    /// Polls the consumer: drains input events, checks buffer-ready, and attempts
    /// a reconnect if we fell back. Called from the poll timer.
    pub fn poll(&mut self, niri: &mut Niri) {
        // Forward compositor clipboard changes to the consumer (main thread so the
        // ffi send never races with enter_fallback / rendering).
        while let Ok(text) = self.clipboard_rx.try_recv() {
            self.send_clipboard_to_consumer(&text);
        }
        // A client set the clipboard: read its text and forward it.
        while let Ok(()) = self.selection_rx.try_recv() {
            self.read_and_forward_clipboard(niri);
        }

        let in_fallback = unsafe { ffi::is_fallback(self.ctx) };
        if in_fallback {
            #[cfg(have_anland_audio)]
            unsafe {
                ffi::anland_audio_set_fd(-1)
            };
            #[cfg(have_anland_audio)]
            unsafe {
                ffi::anland_camera_clear()
            };
            if self.try_reconnect() {
                self.on_reconnect(niri);
            }
            return;
        }

        // Buffer-ready acts as our "vblank": complete any pending frame and unlock
        // rendering for the next one.
        if self.buffer_ready() {
            self.on_buffer_ready(niri);
        }
    }

    /// Ask the client that owns the current clipboard selection for its text/plain
    /// content and forward it to the consumer once it arrives.
    fn read_and_forward_clipboard(&mut self, niri: &mut Niri) {
        use std::os::fd::FromRawFd;

        use smithay::wayland::selection::data_device::request_data_device_client_selection;

        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        let read_fd = fds[0];
        if request_data_device_client_selection(&niri.seat, "text/plain".to_string(), unsafe {
            OwnedFd::from_raw_fd(fds[1])
        })
        .is_err()
        {
            unsafe { libc::close(read_fd) };
            return;
        }

        let tx = self.clipboard_tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let start = std::time::Instant::now();
            loop {
                if start.elapsed() > Duration::from_secs(2) || bytes.len() > 2 * 1024 * 1024 {
                    break;
                }
                let mut pfd = libc::pollfd {
                    fd: read_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let n = unsafe { libc::poll(&mut pfd, 1, 200) };
                if n <= 0 {
                    continue;
                }
                let mut buf = [0u8; 4096];
                let r = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
                if r <= 0 {
                    break; // EOF / error
                }
                bytes.extend_from_slice(&buf[..r as usize]);
            }
            unsafe { libc::close(read_fd) };
            if let Ok(text) = String::from_utf8(bytes) {
                let _ = tx.send(text);
            }
        });
    }

    /// Send a clipboard text to the consumer as an OUTPUT_TYPE_CLIPBOARD event.
    fn send_clipboard_to_consumer(&mut self, text: &str) {
        if unsafe { ffi::is_fallback(self.ctx) } {
            return;
        }
        let bytes = text.as_bytes();
        let ev = ffi::OutputEvent {
            type_: ffi::OUTPUT_TYPE_CLIPBOARD,
            data: ffi::OutputEventData {
                clipboard: ffi::SizeEvent {
                    size: bytes.len() as u32,
                },
            },
        };
        unsafe {
            ffi::push_output_event_with_length(self.ctx, &ev, bytes.as_ptr().cast(), bytes.len());
        }
    }

    fn try_reconnect(&mut self) -> bool {
        if !unsafe { ffi::is_fallback(self.ctx) } {
            return false;
        }
        (unsafe { ffi::try_exit_fallback(self.ctx) }) == 0
    }

    fn on_reconnect(&mut self, niri: &mut Niri) {
        info!("anland: consumer connected");
        #[cfg(have_anland_audio)]
        unsafe {
            ffi::anland_audio_set_fd(ffi::get_audio_fd(self.ctx));
        }
        // Ask the consumer for its camera service fds; it replies asynchronously
        // with an INPUT_TYPE_RESOURCE event (a no-op if the consumer has no camera).
        #[cfg(have_anland_audio)]
        {
            let r = unsafe {
                ffi::push_resources_request(self.ctx, ffi::SERVICE_TYPE_CAMERA, std::ptr::null())
            };
            debug!("anland: camera resource request sent, rc={r}");
        }
        self.consumer_ready = true;
        // Fresh dmabufs from the consumer: force full redraws until every buffer
        // has been rendered at least once.
        self.buffer_last_rendered_frame = [u64::MAX; ffi::MAX_BUFS];
        // Old dmabuf fds belong to the previous consumer session; drop them so
        // the renderer re-imports the new buffers.
        self.consumer_dmabufs = [const { None }; ffi::MAX_BUFS];
        self.input_backend.native_w = self.screen_w;
        self.input_backend.native_h = self.screen_h;
        self.pending_feedback = None;
        if let Some(output) = self.output.clone() {
            niri.queue_redraw(&output);
        }
    }

    /// Polls one input event from the consumer.
    fn poll_input(
        &mut self,
        niri: &mut Niri,
    ) -> Option<smithay::backend::input::InputEvent<input::AnlandInputBackend>> {
        let mut raw = unsafe { mem::zeroed::<ffi::InputEvent>() };
        let r = unsafe { ffi::poll_input_event(self.ctx, &mut raw, 0) };
        match r {
            // -2 = the consumer re-allocated its dmabuf set (resize / occlusion).
            // Drop the cached Dmabufs and force a full redraw so we re-import the
            // fresh buffers instead of writing into freed ones (GPU page faults).
            -2 => {
                info!("anland: consumer re-allocated dmabufs, re-importing");
                self.consumer_dmabufs = [const { None }; ffi::MAX_BUFS];
                self.buffer_last_rendered_frame = [u64::MAX; ffi::MAX_BUFS];
                if let Some(output) = self.output.clone() {
                    niri.queue_redraw(&output);
                }
                None
            }
            1 => {
                let t = raw.type_;
                trace!("anland: INPUT type={t}");
                // Variable-length messages (clipboard / text input) carry a payload
                // after the InputEvent header; it must be drained or it stays in the
                // socket and desyncs the message framing, stalling all input.
                if matches!(t, ffi::INPUT_TYPE_CLIPBOARD | ffi::INPUT_TYPE_TEXT_INPUT) {
                    let size = unsafe { raw.data.clipboard.size };
                    if size > 0 {
                        let mut buf = vec![0u8; size as usize];
                        let r = unsafe {
                            ffi::poll_input_event_extend_data(
                                self.ctx,
                                buf.as_mut_ptr().cast(),
                                size as usize,
                                100,
                            )
                        };
                        if r < 0 {
                            warn!("anland: failed draining input payload");
                        } else if t == ffi::INPUT_TYPE_CLIPBOARD {
                            if let Ok(text) = String::from_utf8(buf) {
                                self.set_compositor_clipboard(niri, text);
                            }
                        }
                    }
                }
                #[cfg(have_anland_audio)]
                if t == ffi::INPUT_TYPE_RESOURCE {
                    let service = unsafe { raw.data.resource.type_ };
                    self.handle_resource_reply(niri, service);
                    return None;
                }
                input::translate(&self.input_backend, &raw)
            }
            -1 => {
                warn!("anland: consumer lost while polling input");
                None
            }
            _ => None,
        }
    }

    /// The consumer replied to a service request with fds (INPUT_TYPE_RESOURCE).
    /// Receive the fds and hand them to the camera engine.
    #[cfg(have_anland_audio)]
    fn handle_resource_reply(&mut self, _niri: &mut Niri, service: u32) {
        let mut fds = [0; 9]; // ctrl + up to MAX_CAMERAS streams
        let fdnum =
            unsafe { ffi::poll_input_event_extend_fds(self.ctx, fds.as_mut_ptr(), 9, 5000) };
        if fdnum < 1 {
            warn!("anland: failed receiving service fds");
            return;
        }
        if service == ffi::SERVICE_TYPE_CAMERA && fdnum >= 2 {
            unsafe {
                ffi::anland_camera_set_resources(fds[0], fds.as_ptr().add(1), fdnum - 1);
            }
        } else {
            for &fd in fds.iter().take(fdnum as usize) {
                unsafe { libc::close(fd) };
            }
        }
    }

    /// Publish text pushed by the consumer as the compositor's clipboard selection.
    fn set_compositor_clipboard(&mut self, niri: &mut Niri, text: String) {
        use smithay::wayland::selection::data_device::set_data_device_selection;
        if text.is_empty() {
            return;
        }
        set_data_device_selection(
            &niri.display_handle,
            &niri.seat,
            vec!["text/plain".to_string()],
            Arc::from(text.into_bytes()),
        );
    }

    /// Drains and returns whether the consumer signalled buffer-ready.
    fn buffer_ready(&mut self) -> bool {
        if unsafe { ffi::is_fallback(self.ctx) } {
            return false;
        }
        let fd = unsafe { ffi::get_buffer_ready_fd(self.ctx) };
        if fd < 0 {
            return false;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, 0) };
        r > 0 && (pfd.revents & libc::POLLIN) != 0
    }

    fn drain_buffer_ready(&mut self) {
        let fd = unsafe { ffi::get_buffer_ready_fd(self.ctx) };
        if fd >= 0 {
            let mut val = 0u64;
            unsafe {
                libc::read(
                    fd,
                    &mut val as *mut u64 as *mut libc::c_void,
                    mem::size_of::<u64>(),
                );
            }
        }
    }

    fn on_buffer_ready(&mut self, niri: &mut Niri) {
        self.drain_buffer_ready();
        self.consumer_ready = true;

        let Some(output) = self.output.clone() else {
            return;
        };
        let Some(output_state) = niri.output_state.get_mut(&output) else {
            return;
        };

        let _redraw_needed = match mem::replace(&mut output_state.redraw_state, RedrawState::Idle) {
            RedrawState::WaitingForVBlank { redraw_needed } => redraw_needed,
            state => {
                error!("anland: unexpected redraw state on buffer ready: {state:?}");
                true
            }
        };

        let now = get_monotonic_time();
        if self.frame_stats_count == 0 {
            self.frame_stats_start = now;
        }
        self.frame_stats_count += 1;
        let elapsed = now.saturating_sub(self.frame_stats_start);
        if elapsed >= Duration::from_secs(2) {
            info!(
                "anland: frame rate = {:.1} fps over {} frames",
                self.frame_stats_count as f64 / (elapsed.as_micros() as f64 / 1_000_000.0),
                self.frame_stats_count
            );
            self.frame_stats_count = 0;
            self.frame_stats_start = Duration::ZERO;
        }
        if let Some(mut feedback) = self.pending_feedback.take() {
            let refresh = output_state
                .frame_clock
                .refresh_interval()
                .map(Refresh::Fixed)
                .unwrap_or(Refresh::Unknown);
            feedback.presented::<_, smithay::utils::Monotonic>(
                now,
                refresh,
                self.frame_seq,
                wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwCompletion,
            );
            output_state.frame_clock.presented(now);
        }

        // The consumer's protocol is lockstep: every select_dmabuf (buffer-ready)
        // is immediately followed by refresh_done, which BLOCKS waiting for exactly
        // one producer render-done message. If we skip rendering, the consumer's
        // refresh_done times out after 5s -> fallback -> reconnect, which shows up
        // as a recurring flicker. Always queue a redraw so every buffer-ready
        // produces exactly one render-done message.
        niri.queue_redraw(&output);
    }

    pub fn render(&mut self, niri: &mut Niri, output: &Output) -> RenderResult {
        let span = tracy_client::span!("Anland::render");

        if !self.consumer_ready {
            trace!("anland: skipping render, consumer not ready");
            return RenderResult::Skipped;
        }
        if unsafe { ffi::is_fallback(self.ctx) } {
            trace!("anland: render SKIP fallback");
            return RenderResult::Skipped;
        }

        let idx = unsafe { ffi::get_selected_idx(self.ctx) };
        if idx < 0 {
            trace!("anland: render SKIP no buffer selected");
            return RenderResult::Skipped;
        }
        let fd = unsafe { ffi::get_dmabuf_fd_at(self.ctx, idx) };
        if fd < 0 {
            trace!("anland: render SKIP no dmabuf at idx {idx}");
            return RenderResult::Skipped;
        }

        let mut info = ffi::buf_info::default();
        if unsafe { ffi::get_dmabuf_info_at(self.ctx, idx, &mut info) } < 0 {
            trace!("anland: render SKIP dmabuf info");
            return RenderResult::Skipped;
        }

        span.emit_text(&format!(
            "buffer {idx} {}x{} stride {}",
            info.width, info.height, info.stride
        ));

        // The consumer may have rotated or resized; if the buffer no longer matches
        // the output, adapt the output to the consumer's current size and continue
        // instead of skipping every frame.
        let size = output
            .current_mode()
            .map(|m| m.size)
            .unwrap_or(Size::from((0, 0)));
        if size.w != info.width as i32 || size.h != info.height as i32 {
            self.adapt_to_size(niri, &output, info.width as i32, info.height as i32);
        }

        let Some(renderer) = self.renderer.as_mut() else {
            error!("anland: renderer not initialized");
            return RenderResult::Skipped;
        };

        let render_start = get_monotonic_time();

        let mut dmabuf = match self.consumer_dmabufs[idx as usize].take() {
            Some(dmabuf) => dmabuf,
            None => {
                let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
                if dup < 0 {
                    error!("anland: error duplicating dmabuf fd");
                    return RenderResult::Skipped;
                }
                let mut builder = Dmabuf::builder(
                    (info.width as i32, info.height as i32),
                    protocol_format_to_fourcc(info.format),
                    Modifier::from(info.modifier),
                    DmabufFlags::empty(),
                );
                builder.add_plane(
                    unsafe { OwnedFd::from_raw_fd(dup) },
                    info.offset,
                    info.stride,
                );
                match builder.build() {
                    Some(dmabuf) => dmabuf,
                    None => {
                        error!("anland: failed to build dmabuf");
                        return RenderResult::Skipped;
                    }
                }
            }
        };

        // Build the render elements for this output.
        let ctx = RenderCtx {
            renderer: &mut *renderer,
            target: RenderTarget::Output,
            xray: None,
        };
        let elements = niri.render_to_vec(ctx, output, true);

        // Buffer age for the damage tracker: how many consumer frames ago this
        // dmabuf was last rendered into. age 0 forces a full redraw (and a
        // re-read of every client buffer), which makes slow software (SHM)
        // clients flicker. A real age lets the damage tracker redraw only what
        // actually changed.
        let frame = self.frame_seq;
        let age = if self.buffer_last_rendered_frame[idx as usize] == u64::MAX {
            0
        } else {
            frame.wrapping_sub(self.buffer_last_rendered_frame[idx as usize]) as usize
        };
        self.buffer_last_rendered_frame[idx as usize] = frame;

        // Render them into the consumer's dmabuf.
        let damage_tracker = self.damage_tracker.as_mut().unwrap();
        let mut target = match renderer.bind(&mut dmabuf) {
            Ok(target) => target,
            Err(err) => {
                warn!("anland: error binding dmabuf: {err}");
                return RenderResult::Skipped;
            }
        };
        let res = match damage_tracker.render_output(
            renderer,
            &mut target,
            age,
            &elements,
            Color32F::TRANSPARENT,
        ) {
            Ok(res) => res,
            Err(err) => {
                warn!("anland: error rendering to dmabuf: {err}");
                return RenderResult::Skipped;
            }
        };
        let RenderOutputResult { sync, states, .. } = res;

        // The target borrows `dmabuf`; drop it now so we can store the dmabuf
        // back into the cache below.
        drop(target);

        let render_dur = get_monotonic_time().saturating_sub(render_start);
        if render_dur > Duration::from_millis(30) {
            info!("anland: SLOW render took {render_dur:?}");
        }

        niri.update_primary_scanout_output(output, &states);

        // Hand a native fence to the consumer so it can wait on the render
        // GPU-side. freedreno/KGSL advertises EGL_ANDROID_native_fence_sync but
        // eglDupNativeFenceFDANDROID fails at runtime (EGL_BAD_PARAMETER), so
        // export() comes up empty even when is_exportable() is true. Without a
        // real fence the consumer presents a half-rendered buffer → flicker /
        // corruption under complex composites. Work around it by blocking on the
        // render fence CPU-side before handing the buffer over.
        let fence_fd = if sync.is_exportable() {
            match sync.export() {
                Some(fd) => fd.into_raw_fd(),
                None => {
                    let _ = sync.wait();
                    -1
                }
            }
        } else {
            let _ = sync.wait();
            -1
        };
        unsafe { ffi::set_render_fence(self.ctx, fence_fd) };
        unsafe { ffi::trigger_refresh(self.ctx) };

        // Collect presentation feedback and complete it when the consumer signals
        // buffer-ready.
        self.pending_feedback = Some(niri.take_presentation_feedbacks(output, &states));
        self.consumer_ready = false;

        // Mark the frame as in flight. Buffer-ready completes it.
        let output_state = niri.output_state.get_mut(output).unwrap();
        match mem::replace(
            &mut output_state.redraw_state,
            RedrawState::WaitingForVBlank {
                redraw_needed: false,
            },
        ) {
            RedrawState::Idle => unreachable!(),
            RedrawState::Queued => (),
            RedrawState::WaitingForVBlank { .. } => unreachable!(),
            RedrawState::WaitingForEstimatedVBlank(_) => unreachable!(),
            RedrawState::WaitingForEstimatedVBlankAndQueued(token) => {
                niri.event_loop.remove(token);
            }
        }
        output_state.frame_callback_sequence = output_state.frame_callback_sequence.wrapping_add(1);
        self.frame_seq = self.frame_seq.wrapping_add(1);

        // Keep the dmabuf (and thus the renderer's cached EGL image / texture /
        // renderbuffer for it) alive for the next time this buffer index is
        // selected, so we don't recreate EGL images every frame.
        self.consumer_dmabufs[idx as usize] = Some(dmabuf);

        RenderResult::Submitted
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        // Import client dmabufs through the GL renderer so the compositor can
        // texture them. (Direct scanout is still not possible: the consumer owns
        // all the buffers.) Returning false here made every client dmabuf buffer
        // fail with a fatal protocol error, forcing clients onto SHM buffers.
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        match renderer.import_dmabuf(dmabuf, None) {
            Ok(_texture) => true,
            Err(err) => {
                debug!("anland: error importing dmabuf: {err:?}");
                false
            }
        }
    }

    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        self.ipc_outputs.clone()
    }
}

impl Drop for Anland {
    fn drop(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        #[cfg(have_anland_audio)]
        unsafe {
            ffi::anland_audio_stop();
        }
        #[cfg(have_anland_audio)]
        unsafe {
            ffi::anland_camera_stop();
        }
        if !self.ctx.is_null() {
            unsafe { ffi::disconnect(self.ctx) };
            self.ctx = std::ptr::null_mut();
        }
    }
}
