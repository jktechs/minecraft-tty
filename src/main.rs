use std::{collections::HashMap, time::Duration};

use calloop::EventLoop;

use calloop::ping::make_ping;
use launcher::types::{FeatureRule, Os, OsRule, Rule, RuleAction};
use smithay::{delegate_dmabuf, reexports::wayland_server::Display, wayland::seat::WaylandFocus};

use crate::{state::App, udev::UdevBackend};

mod base;
mod seat;
mod state;
mod udev;
mod xdg;

struct LoopData {
    display: Display<App>,
    app: App,
    backend: UdevBackend,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("started");
    let (request_redraw, redraw) = make_ping()?;
    let mut display: Display<App> = Display::new()?;
    let dh = &display.handle();
    let mut backend = UdevBackend::new(redraw)?;

    let mut app = App::new(dh, request_redraw, Some(&backend.render_feedback))?;

    let mut event_loop = EventLoop::<LoopData>::try_new()?;

    let mc_exit_signal = event_loop.get_signal();

    std::thread::spawn(move || {
        let quickplay = std::fs::exists("./instance/saves/New World").unwrap();
        let output = launcher::run_sync(
            Rule {
                action: RuleAction::Allow,
                features: Some(FeatureRule {
                    has_custom_resolution: Some(false),
                    has_quick_plays_support: Some(false),
                    is_demo_user: Some(false),
                    is_quick_play_multiplayer: Some(false),
                    is_quick_play_realms: Some(false),
                    is_quick_play_singleplayer: Some(quickplay),
                }),
                os: Some(OsRule {
                    arch: Some("x86_64".into()),
                    name: Some(Os::Linux),
                    version: None,
                }),
            },
            "26.3".into(),
            "26.3.0.41-beta".into(),
            HashMap::from([
                ("quickPlayPath", "./instance".into()),
                ("quickPlaySingleplayer", "New World".into()),
            ]),
        )
        .unwrap()
        .env("WAYLAND_DISPLAY", "wayland-5")
        .env("SDL_VIDEODRIVER", "wayland")
        .output()
        .unwrap();
        if !output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            let err = String::from_utf8_lossy(&output.stderr);
            panic!("stdout:\n{out}\nstderr:\n{err}");
        }
        mc_exit_signal.stop();
    });

    backend.register_loop(&event_loop)?;
    app.register_loop(&event_loop, &mut display)?;

    event_loop.run(
        Duration::from_millis(20),
        &mut LoopData {
            display,
            app,
            backend,
        },
        |loop_data| {
            loop_data.backend.import_pending(&mut loop_data.app);
            loop_data.display.flush_clients().unwrap();

            let surface = loop_data.app.surface.as_ref().and_then(|x| x.wl_surface());
            if !loop_data.app.seat_state.pointer_locked(surface.as_deref())
                && let Some(position) = loop_data.app.seat_state.position_hint.take()
            {
                loop_data.app.seat_state.pointer.set_location(position);
            }
        },
    )?;
    Ok(())
}

// impl DrmSyncobjHandler for App {
//     fn drm_syncobj_state(&mut self) -> Option<&mut smithay::wayland::drm_syncobj::DrmSyncobjState> {
//         None
//     }
// }
impl smithay::wayland::dmabuf::DmabufHandler for App {
    fn dmabuf_imported(
        &mut self,
        global: &smithay::wayland::dmabuf::DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: smithay::wayland::dmabuf::ImportNotifier,
    ) {
        if self.dmabuf_global.is_none_or(|g| g == *global) {
            self.pending_dmabufs.push((dmabuf, notifier));
        }
    }
    fn dmabuf_state(&mut self) -> &mut smithay::wayland::dmabuf::DmabufState {
        &mut self.dmabuf_state
    }
}
// let plane_formats = self.compositor.surface().plane_info().formats.clone(); // primary plane
// let scanout = DmabufFeedbackBuilder::new(dev, self.renderer.dmabuf_formats())
//     .add_preference_tranche(dev, Some(TrancheFlags::Scanout), plane_formats)
//     .build()?;
// delegate_drm_syncobj!(App);
// delegate_presentation!(App);
delegate_dmabuf!(App);
