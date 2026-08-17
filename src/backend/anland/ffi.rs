//! Raw FFI bindings to the anland `libdisplay_producer` C library.

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::os::raw::{c_char, c_int, c_void};

pub const MAX_BUFS: usize = 8;

// protocol.h input event types
pub const INPUT_TYPE_TOUCH: u32 = 1;
pub const INPUT_TYPE_KEY: u32 = 2;
pub const INPUT_TYPE_POINTER_MOTION: u32 = 3;
pub const INPUT_TYPE_POINTER_BUTTON: u32 = 4;
pub const INPUT_TYPE_POINTER_AXIS: u32 = 5;
pub const INPUT_TYPE_TOUCH_FRAME: u32 = 6;
pub const INPUT_TYPE_DISPLAY_REFRESH: u32 = 7;
pub const INPUT_TYPE_CLIPBOARD: u32 = 8;
pub const INPUT_TYPE_TEXT_INPUT: u32 = 9;
pub const INPUT_TYPE_ACTION: u32 = 10;
pub const INPUT_TYPE_RESOURCE: u32 = 11;
pub const INPUT_TYPE_RESOURCE_INVALID: u32 = 12;

pub const INPUT_ACTION_DOWN: i32 = 0;
pub const INPUT_ACTION_UP: i32 = 1;
pub const INPUT_ACTION_MOVE: i32 = 2;

// protocol.h output event types (producer -> consumer)
pub const OUTPUT_TYPE_CLIPBOARD: u32 = 1;
pub const OUTPUT_TYPE_RESOURCES_REQUEST: u32 = 2;
pub const OUTPUT_TYPE_SET_CONSUMER_VAR: u32 = 3;

pub const SERVICE_TYPE_CAMERA: u32 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct buf_info {
    pub stride: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub modifier: u64,
    pub offset: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TouchEvent {
    pub action: i32,
    pub x: f32,
    pub y: f32,
    pub pointer_id: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub action: i32,
    pub keycode: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PointerMotionEvent {
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PointerButtonEvent {
    pub button: u32,
    pub pressed: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PointerAxisEvent {
    pub axis: u32,
    pub value: f32,
    pub discrete: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DisplayEvent {
    pub refresh_mhz: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SizeEvent {
    pub size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputActionEvent {
    pub action: u32,
    pub value: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ResourceEvent {
    pub type_: u32,
    pub fdnum: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union InputEventData {
    pub touch: TouchEvent,
    pub key: KeyEvent,
    pub pointer_motion: PointerMotionEvent,
    pub pointer_button: PointerButtonEvent,
    pub pointer_axis: PointerAxisEvent,
    pub display: DisplayEvent,
    pub clipboard: SizeEvent,
    pub text_input: SizeEvent,
    pub input_action: InputActionEvent,
    pub resource: ResourceEvent,
    pub padding: [u32; 4],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct InputEvent {
    pub type_: u32,
    pub data: InputEventData,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResourceRequestEvent {
    pub type_: u32,
    pub args: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SetConsumerVarEvent {
    pub var: u32,
    pub value: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union OutputEventData {
    pub clipboard: SizeEvent,
    pub resources_request: ResourceRequestEvent,
    pub set_consumer_var: SetConsumerVarEvent,
    pub padding: [u32; 4],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct OutputEvent {
    pub type_: u32,
    pub data: OutputEventData,
}

#[repr(C)]
pub struct display_ctx {
    _private: [u8; 0],
}

extern "C" {
    pub fn connect_to_deamon(ctx: *mut *mut display_ctx, socket_path: *const c_char) -> c_int;
    pub fn disconnect(ctx: *mut display_ctx);
    pub fn get_screen_info(
        ctx: *mut display_ctx,
        width: *mut u32,
        height: *mut u32,
        format: *mut u32,
        refresh: *mut u32,
    ) -> c_int;
    pub fn set_render_fence(ctx: *mut display_ctx, fence_fd: c_int);
    pub fn trigger_refresh(ctx: *mut display_ctx) -> c_int;
    pub fn poll_input_event(
        ctx: *mut display_ctx,
        event: *mut InputEvent,
        timeout_ms: c_int,
    ) -> c_int;
    pub fn poll_input_event_extend_data(
        ctx: *mut display_ctx,
        payload: *mut c_void,
        size: usize,
        timeout_ms: c_int,
    ) -> c_int;
    pub fn set_fallback_callback(
        ctx: *mut display_ctx,
        on_fallback: Option<unsafe extern "C" fn(*mut c_void)>,
        userdata: *mut c_void,
    ) -> c_int;
    pub fn is_fallback(ctx: *mut display_ctx) -> bool;
    pub fn try_exit_fallback(ctx: *mut display_ctx) -> c_int;
    pub fn get_data_fd(ctx: *mut display_ctx) -> c_int;
    pub fn get_buffer_ready_fd(ctx: *mut display_ctx) -> c_int;
    pub fn get_buf_count(ctx: *mut display_ctx) -> c_int;
    pub fn get_selected_idx(ctx: *mut display_ctx) -> c_int;
    pub fn get_dmabuf_fd_at(ctx: *mut display_ctx, idx: c_int) -> c_int;
    pub fn get_dmabuf_info_at(ctx: *mut display_ctx, idx: c_int, info: *mut buf_info) -> c_int;
    pub fn get_audio_fd(ctx: *mut display_ctx) -> c_int;
    pub fn push_output_event_with_length(
        ctx: *mut display_ctx,
        event: *const OutputEvent,
        payload: *const c_void,
        size: usize,
    ) -> c_int;
    pub fn push_resources_request(
        ctx: *mut display_ctx,
        service_type: u32,
        args: *const u32,
    ) -> c_int;
    pub fn poll_input_event_extend_fds(
        ctx: *mut display_ctx,
        fds: *mut c_int,
        fd_count: c_int,
        timeout_ms: c_int,
    ) -> c_int;
    #[cfg(have_anland_audio)]
    pub fn anland_audio_start() -> c_int;
    #[cfg(have_anland_audio)]
    pub fn anland_audio_stop();
    #[cfg(have_anland_audio)]
    pub fn anland_audio_set_fd(audio_fd: c_int);
    #[cfg(have_anland_audio)]
    pub fn anland_camera_start() -> c_int;
    #[cfg(have_anland_audio)]
    pub fn anland_camera_stop();
    #[cfg(have_anland_audio)]
    pub fn anland_camera_set_resources(
        ctrl_fd: c_int,
        stream_fds: *const c_int,
        num_cameras: c_int,
    );
    #[cfg(have_anland_audio)]
    pub fn anland_camera_clear();
}
