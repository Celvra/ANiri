//! Anland input backend: feeds smithay input events from the anland consumer.

use std::path::PathBuf;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Device,
    DeviceCapability, Event, InputBackend, KeyState, KeyboardKeyEvent, PointerAxisEvent,
    PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent, TouchDownEvent,
    TouchFrameEvent, TouchMotionEvent, TouchUpEvent, TouchEvent, TouchSlot,
};
use smithay::backend::input::Keycode;
use smithay::output::Output;

use super::ffi;
use crate::input::backend_ext::NiriInputDevice;
use crate::niri::State;

/// A single virtual input device that aggregates keyboard, pointer and touch from
/// the anland consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnlandDevice;

impl Device for AnlandDevice {
    fn id(&self) -> String {
        "anland".to_string()
    }

    fn name(&self) -> String {
        "anland consumer input".to_string()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(
            capability,
            DeviceCapability::Keyboard | DeviceCapability::Pointer | DeviceCapability::Touch
        )
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<PathBuf> {
        None
    }
}

impl NiriInputDevice for AnlandDevice {
    fn output(&self, _state: &State) -> Option<Output> {
        // Single-output backend. Leave the cursor in *global* output coordinates,
        // same as the winit backend.
        None
    }
}

/// The consumer reports coordinates in its native (physical) pixel space, which may
/// differ from the buffer size (e.g. after rotation). We scale absolute positions
/// into the target coordinate space from the consumer's native extent.
#[derive(Debug, Clone, Copy)]
pub struct AnlandEventBase {
    pub time_usec: u64,
    pub native_w: u32,
    pub native_h: u32,
    pub x: f64,
    pub y: f64,
}

impl AnlandEventBase {
    fn x_transformed(&self, width: i32) -> f64 {
        if self.native_w == 0 {
            return self.x;
        }
        self.x * (width as f64) / (self.native_w as f64)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        if self.native_h == 0 {
            return self.y;
        }
        self.y * (height as f64) / (self.native_h as f64)
    }
}

impl Event<AnlandInputBackend> for AnlandEventBase {
    fn time(&self) -> u64 {
        self.time_usec
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl AbsolutePositionEvent<AnlandInputBackend> for AnlandEventBase {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, width: i32) -> f64 {
        AnlandEventBase::x_transformed(self, width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        AnlandEventBase::y_transformed(self, height)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnlandKeyboardKeyEvent {
    pub base: AnlandEventBase,
    pub keycode: u32,
    pub state: KeyState,
}

impl Event<AnlandInputBackend> for AnlandKeyboardKeyEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl KeyboardKeyEvent<AnlandInputBackend> for AnlandKeyboardKeyEvent {
    fn key_code(&self) -> Keycode {
        // smithay/libinput convert evdev keycodes into xkb keycodes by adding 8.
        Keycode::from(self.keycode + 8)
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        1
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerMotionEvent {
    pub base: AnlandEventBase,
    pub dx: f64,
    pub dy: f64,
}

impl Event<AnlandInputBackend> for AnlandPointerMotionEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl PointerMotionEvent<AnlandInputBackend> for AnlandPointerMotionEvent {
    fn delta_x(&self) -> f64 {
        self.dx
    }

    fn delta_y(&self) -> f64 {
        self.dy
    }

    fn delta_x_unaccel(&self) -> f64 {
        self.dx
    }

    fn delta_y_unaccel(&self) -> f64 {
        self.dy
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerMotionAbsoluteEvent(pub AnlandEventBase);

impl Event<AnlandInputBackend> for AnlandPointerMotionAbsoluteEvent {
    fn time(&self) -> u64 {
        self.0.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl AbsolutePositionEvent<AnlandInputBackend> for AnlandPointerMotionAbsoluteEvent {
    fn x(&self) -> f64 {
        self.0.x()
    }

    fn y(&self) -> f64 {
        self.0.y()
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.0.x_transformed(width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.0.y_transformed(height)
    }
}

impl PointerMotionAbsoluteEvent<AnlandInputBackend> for AnlandPointerMotionAbsoluteEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerButtonEvent {
    pub base: AnlandEventBase,
    pub button: u32,
    pub state: ButtonState,
}

impl Event<AnlandInputBackend> for AnlandPointerButtonEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl PointerButtonEvent<AnlandInputBackend> for AnlandPointerButtonEvent {
    fn button_code(&self) -> u32 {
        self.button
    }

    fn state(&self) -> ButtonState {
        self.state
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnlandPointerAxisEvent {
    pub base: AnlandEventBase,
    pub axis: u32,
    pub value: f64,
    pub discrete: i32,
}

impl Event<AnlandInputBackend> for AnlandPointerAxisEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl PointerAxisEvent<AnlandInputBackend> for AnlandPointerAxisEvent {
    fn amount(&self, axis: Axis) -> Option<f64> {
        match (self.axis, axis) {
            (0, Axis::Vertical) | (1, Axis::Horizontal) => Some(self.value),
            _ => None,
        }
    }

    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        match (self.axis, axis) {
            (0, Axis::Vertical) | (1, Axis::Horizontal) => Some(f64::from(self.discrete) * 120.),
            _ => None,
        }
    }

    fn source(&self) -> AxisSource {
        AxisSource::Wheel
    }

    fn relative_direction(&self, _axis: Axis) -> AxisRelativeDirection {
        AxisRelativeDirection::Identical
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchDownEvent {
    pub base: AnlandEventBase,
    pub slot: TouchSlot,
}

impl Event<AnlandInputBackend> for AnlandTouchDownEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl TouchEvent<AnlandInputBackend> for AnlandTouchDownEvent {
    fn slot(&self) -> TouchSlot {
        self.slot
    }
}

impl AbsolutePositionEvent<AnlandInputBackend> for AnlandTouchDownEvent {
    fn x(&self) -> f64 {
        self.base.x()
    }

    fn y(&self) -> f64 {
        self.base.y()
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.base.x_transformed(width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.base.y_transformed(height)
    }
}

impl TouchDownEvent<AnlandInputBackend> for AnlandTouchDownEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchMotionEvent {
    pub base: AnlandEventBase,
    pub slot: TouchSlot,
}

impl Event<AnlandInputBackend> for AnlandTouchMotionEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl TouchEvent<AnlandInputBackend> for AnlandTouchMotionEvent {
    fn slot(&self) -> TouchSlot {
        self.slot
    }
}

impl AbsolutePositionEvent<AnlandInputBackend> for AnlandTouchMotionEvent {
    fn x(&self) -> f64 {
        self.base.x()
    }

    fn y(&self) -> f64 {
        self.base.y()
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.base.x_transformed(width)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.base.y_transformed(height)
    }
}

impl TouchMotionEvent<AnlandInputBackend> for AnlandTouchMotionEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchUpEvent {
    pub base: AnlandEventBase,
    pub slot: TouchSlot,
}

impl Event<AnlandInputBackend> for AnlandTouchUpEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl TouchEvent<AnlandInputBackend> for AnlandTouchUpEvent {
    fn slot(&self) -> TouchSlot {
        self.slot
    }
}

impl TouchUpEvent<AnlandInputBackend> for AnlandTouchUpEvent {}

#[derive(Debug, Clone, Copy)]
pub struct AnlandTouchFrameEvent {
    pub base: AnlandEventBase,
}

impl Event<AnlandInputBackend> for AnlandTouchFrameEvent {
    fn time(&self) -> u64 {
        self.base.time()
    }

    fn device(&self) -> AnlandDevice {
        AnlandDevice
    }
}

impl TouchFrameEvent<AnlandInputBackend> for AnlandTouchFrameEvent {}

/// The anland input backend. It never produces events on its own; events are pushed
/// by `poll()` from the consumer.
#[derive(Debug)]
pub struct AnlandInputBackend {
    pub native_w: u32,
    pub native_h: u32,
    pub time_offset: u64,
}

impl Default for AnlandInputBackend {
    fn default() -> Self {
        Self {
            native_w: 0,
            native_h: 0,
            time_offset: 0,
        }
    }
}

impl AnlandInputBackend {
    fn base(&self, x: f64, y: f64) -> AnlandEventBase {
        let now = crate::utils::get_monotonic_time().as_micros() as u64;
        AnlandEventBase {
            time_usec: now.saturating_sub(self.time_offset),
            native_w: self.native_w,
            native_h: self.native_h,
            x,
            y,
        }
    }
}

impl InputBackend for AnlandInputBackend {
    type Device = AnlandDevice;
    type KeyboardKeyEvent = AnlandKeyboardKeyEvent;
    type PointerAxisEvent = AnlandPointerAxisEvent;
    type PointerButtonEvent = AnlandPointerButtonEvent;
    type PointerMotionEvent = AnlandPointerMotionEvent;
    type PointerMotionAbsoluteEvent = AnlandPointerMotionAbsoluteEvent;
    type GestureSwipeBeginEvent = smithay::backend::input::UnusedEvent;
    type GestureSwipeUpdateEvent = smithay::backend::input::UnusedEvent;
    type GestureSwipeEndEvent = smithay::backend::input::UnusedEvent;
    type GesturePinchBeginEvent = smithay::backend::input::UnusedEvent;
    type GesturePinchUpdateEvent = smithay::backend::input::UnusedEvent;
    type GesturePinchEndEvent = smithay::backend::input::UnusedEvent;
    type GestureHoldBeginEvent = smithay::backend::input::UnusedEvent;
    type GestureHoldEndEvent = smithay::backend::input::UnusedEvent;
    type TouchDownEvent = AnlandTouchDownEvent;
    type TouchUpEvent = AnlandTouchUpEvent;
    type TouchMotionEvent = AnlandTouchMotionEvent;
    type TouchCancelEvent = smithay::backend::input::UnusedEvent;
    type TouchFrameEvent = AnlandTouchFrameEvent;
    type TabletToolAxisEvent = smithay::backend::input::UnusedEvent;
    type TabletToolProximityEvent = smithay::backend::input::UnusedEvent;
    type TabletToolTipEvent = smithay::backend::input::UnusedEvent;
    type TabletToolButtonEvent = smithay::backend::input::UnusedEvent;
    type SwitchToggleEvent = smithay::backend::input::UnusedEvent;
    type SpecialEvent = ();
}

/// Convert a raw anland `InputEvent` into a smithay `InputEvent`.
pub fn translate(backend: &AnlandInputBackend, raw: &ffi::InputEvent) -> Option<smithay::backend::input::InputEvent<AnlandInputBackend>> {
    use smithay::backend::input::{InputEvent as I, KeyState, ButtonState};

    let base = |x: f64, y: f64| backend.base(x, y);

    unsafe {
        match raw.type_ {
            ffi::INPUT_TYPE_KEY => {
                let key = raw.data.key;
                Some(I::Keyboard {
                    event: AnlandKeyboardKeyEvent {
                        base: base(0., 0.),
                        keycode: key.keycode.max(0) as u32,
                        state: if key.action == ffi::INPUT_ACTION_DOWN {
                            KeyState::Pressed
                        } else {
                            KeyState::Released
                        },
                    },
                })
            }
            ffi::INPUT_TYPE_POINTER_MOTION => {
                let m = raw.data.pointer_motion;
                // The consumer provides an absolute position alongside the deltas,
                // so report it as an absolute motion to keep the cursor in sync.
                Some(I::PointerMotionAbsolute {
                    event: AnlandPointerMotionAbsoluteEvent(base(f64::from(m.x), f64::from(m.y))),
                })
            }
            ffi::INPUT_TYPE_POINTER_BUTTON => {
                let b = raw.data.pointer_button;
                Some(I::PointerButton {
                    event: AnlandPointerButtonEvent {
                        base: base(0., 0.),
                        button: b.button,
                        state: if b.pressed != 0 {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                    },
                })
            }
            ffi::INPUT_TYPE_POINTER_AXIS => {
                let a = raw.data.pointer_axis;
                Some(I::PointerAxis {
                    event: AnlandPointerAxisEvent {
                        base: base(0., 0.),
                        axis: a.axis,
                        value: f64::from(a.value),
                        discrete: a.discrete,
                    },
                })
            }
            ffi::INPUT_TYPE_TOUCH => {
                let t = raw.data.touch;
                let slot = TouchSlot::from(Some(t.pointer_id.max(0) as u32));
                match t.action {
                    ffi::INPUT_ACTION_DOWN => Some(I::TouchDown {
                        event: AnlandTouchDownEvent {
                            base: base(f64::from(t.x), f64::from(t.y)),
                            slot,
                        },
                    }),
                    ffi::INPUT_ACTION_MOVE => Some(I::TouchMotion {
                        event: AnlandTouchMotionEvent {
                            base: base(f64::from(t.x), f64::from(t.y)),
                            slot,
                        },
                    }),
                    ffi::INPUT_ACTION_UP => Some(I::TouchUp {
                        event: AnlandTouchUpEvent { base: base(f64::from(t.x), f64::from(t.y)), slot },
                    }),
                    _ => None,
                }
            }
            ffi::INPUT_TYPE_TOUCH_FRAME => Some(I::TouchFrame {
                event: AnlandTouchFrameEvent { base: base(0., 0.) },
            }),
            _ => None,
        }
    }
}
