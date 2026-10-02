use std::time::Duration;

use calloop::{LoopHandle, ping::PingSource};
use smithay::{
    backend::{
        renderer::{
            Frame, Renderer,
            element::{AsRenderElements, surface::WaylandSurfaceRenderElement},
            gles::GlesRenderer,
            utils::draw_render_elements,
        },
        winit::{WinitEvent, WinitEventLoop, WinitGraphicsBackend},
    },
    input::pointer::CursorImageStatus,
    reexports::winit::window::CursorGrabMode,
    utils::{Physical, Point, Scale},
    wayland::seat::WaylandFocus,
};

use crate::{
    LoopData,
    state::{App, WindowingBackend},
};

#[derive(Debug)]
pub struct WinitBackend {
    pub graphics: WinitGraphicsBackend<GlesRenderer>,
    pub sources: Option<(WinitEventLoop, PingSource)>,
}

impl WindowingBackend for WinitBackend {
    fn register_loop(
        &mut self,
        loop_handle: &LoopHandle<'_, LoopData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (event_loop, redraw) = self.sources.take().unwrap();
        loop_handle.insert_source(event_loop, |event, _, loop_data| match event {
            WinitEvent::Resized { size, .. } => loop_data.app.resize(size),
            WinitEvent::Focus(_) => {}
            WinitEvent::Input(event) => loop_data.backend.input(&mut loop_data.app, event),
            WinitEvent::CloseRequested => loop_data.app.exit(),
            WinitEvent::Redraw => loop_data.backend.redraw(&mut loop_data.app).unwrap(),
        })?;
        loop_handle.insert_source(redraw, |_, _, loop_data| {
            loop_data.backend.request_draw().unwrap();
        })?;
        Ok(())
    }
    fn new(redraw: PingSource) -> Result<Self, Box<dyn std::error::Error>> {
        let (graphics, event_loop) = smithay::backend::winit::init::<GlesRenderer>()?;
        Ok(Self {
            sources: Some((event_loop, redraw)),
            graphics,
        })
    }
    fn redraw(&mut self, state: &mut App) -> Result<(), Box<dyn std::error::Error>> {
        let size = self.graphics.window_size();
        let (renderer, mut target) = self.graphics.bind()?;

        let elms = if let Some(surface) = state.surface.as_ref() {
            surface.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                renderer,
                Point::<i32, Physical>::from((0, 0)) - surface.geometry().loc.to_physical(1),
                Scale { x: 1., y: 1. },
                1.,
            )
        } else {
            Vec::new()
        };

        // Clear background to a dark grey canvas
        let mut frame =
            renderer.render(&mut target, size, smithay::utils::Transform::Flipped180)?;
        frame.clear(
            [0.5, 0.1, 0.1, 1.0].into(),
            &[smithay::utils::Rectangle::from_size(size)],
        )?;
        draw_render_elements(
            &mut frame,
            1.,
            &elms,
            &[smithay::utils::Rectangle::from_size(size)],
        )?;
        frame.finish()?.wait()?;

        drop(target);
        self.graphics.submit(None)?;

        if let Some(window) = state.surface.as_ref() {
            window.send_frame(&state.output, Duration::from_secs(4), None, |_, _| {
                Some(state.output.clone())
            });
        }
        Result::<(), Box<dyn std::error::Error>>::Ok(())
    }
    fn cursor(&mut self, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
        let w = self.graphics.window();
        w.set_cursor_visible(!matches!(
            app.seat_state.image_status,
            CursorImageStatus::Hidden
        ));
        let surface = app.surface.as_ref().and_then(|x| x.wl_surface());
        let _ = if app.seat_state.pointer_locked(surface.as_deref()) {
            // Locked is supported on Wayland hosts, Confined on X11; try both.
            w.set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined))
        } else {
            w.set_cursor_grab(CursorGrabMode::None)
        };
        Ok(())
    }
    fn request_draw(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.graphics.window().request_redraw();
        Ok(())
    }
}
