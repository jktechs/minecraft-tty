use smithay::{
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::Window,
    reexports::wayland_protocols::xdg::{
        decoration::zv1::server::zxdg_toplevel_decoration_v1, shell::server::xdg_toplevel,
    },
    utils::{SERIAL_COUNTER, Serial},
    wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        decoration::XdgDecorationHandler,
    },
};
use wayland_server::protocol::wl_seat::WlSeat;

use crate::state::App;

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        if self.surface.is_some() {
            return;
        }
        let size = self.output.current_mode().map(|m| m.size.to_logical(1));
        surface.with_pending_state(|s| {
            s.size = size;
            // Without a state hint, many clients treat size as a suggestion and ignore it.
            s.states.set(xdg_toplevel::State::Fullscreen);
            s.states.set(xdg_toplevel::State::Activated);
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        // existing code: build the Window, store it in self.surface, map it, etc.
        // The initial configure is sent from your commit handler or here; either way
        // the pending state above is what it carries.
        let window = Window::new_wayland_window(surface.clone());
        self.surface = Some(window);
        self.seat_state.keyboard.clone().set_focus(
            self,
            Some(surface.wl_surface().clone()),
            SERIAL_COUNTER.next_serial(),
        );
        surface.send_configure();
    }
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        println!("popups not supported");
    }
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        println!("popups not supported");
    }
    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

delegate_xdg_shell!(App);
impl XdgDecorationHandler for App {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide)
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: zxdg_toplevel_decoration_v1::Mode) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(mode));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide)
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
delegate_xdg_decoration!(App);
