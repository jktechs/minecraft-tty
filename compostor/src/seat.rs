use smithay::{
    delegate_cursor_shape, delegate_pointer_constraints, delegate_relative_pointer, delegate_seat,
    input::{
        SeatHandler, SeatState as WaylandSeatState,
        keyboard::XkbConfig,
        pointer::{CursorIcon, CursorImageStatus, PointerHandle},
    },
    utils::{Logical, Point},
    wayland::{
        pointer_constraints::{PointerConstraintsHandler, with_pointer_constraint},
        tablet_manager::TabletSeatHandler,
    },
};
use wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface};

use crate::state::{App, SeatState};

impl SeatState {
    pub fn new(dh: &DisplayHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let mut wayland_seat_state = WaylandSeatState::new();
        let mut seat = wayland_seat_state.new_wl_seat(dh, "seat-0");
        let keyboard = seat.add_keyboard(XkbConfig::default(), 200, 25)?;
        let pointer = seat.add_pointer();
        Ok(Self {
            wayland_seat_state,
            seat,
            keyboard,
            pointer,
            position_hint: None,
            image_status: smithay::input::pointer::CursorImageStatus::Named(CursorIcon::Default),
        })
    }
    pub fn pointer_locked(&self, surface: Option<&WlSurface>) -> bool {
        surface.is_some_and(|s| {
            with_pointer_constraint(s, &self.pointer, |c| c.is_some_and(|c| c.is_active()))
        })
    }
}
impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut WaylandSeatState<Self> {
        &mut self.seat_state.wayland_seat_state
    }
    fn cursor_image(&mut self, _seat: &smithay::input::Seat<Self>, image: CursorImageStatus) {
        self.seat_state.image_status = image;
    }
}
delegate_seat!(App);
impl PointerConstraintsHandler for App {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        let focused = pointer.current_focus().as_ref() == Some(surface);
        if focused {
            with_pointer_constraint(surface, pointer, |c| {
                if let Some(c) = c
                    && !c.is_active()
                {
                    c.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        self.seat_state.position_hint = Some(location)
    }
}
delegate_relative_pointer!(App);
delegate_pointer_constraints!(App);

impl TabletSeatHandler for App {
    fn tablet_tool_image(
        &mut self,
        _tool: &smithay::backend::input::TabletToolDescriptor,
        _image: CursorImageStatus,
    ) {
        dbg!("Tablet tool cursor changes not supported");
    }
}
delegate_cursor_shape!(App);
