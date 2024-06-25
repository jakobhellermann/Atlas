#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Duration;

use celesteloader::{cct_physics_inspector::PhysicsInspector, CelesteInstallation};
use slint::{ComponentHandle, ModelRc};

mod record_tas;
mod recordings;
mod render;

slint::include_modules!();

pub fn main() {
    let main_window = MainWindow::new().unwrap();

    let celeste = match CelesteInstallation::detect() {
        Ok(celeste) => celeste,
        Err(_) => {
            main_window.set_error("Could not find celeste installation. Please open an issue at https://github.com/jakobhellermann/Atlas or dm me on discord (@dubisteinkek)".into());
            main_window.run().unwrap();
            return;
        }
    };
    let physics_inspector = PhysicsInspector::new(&celeste);

    let main_window = MainWindow::new().unwrap();

    let (recordings_unfiltered, filter_model) =
        recordings::load_model(&main_window, &physics_inspector);

    let _watcher =
        match recordings::watcher::start_watcher(&physics_inspector, main_window.as_weak()) {
            Ok(watcher) => Some(watcher),
            Err(e) => {
                main_window.set_error(
                    format!("Cannot listen to CCT changes in the background: {e:?}").into(),
                );
                None
            }
        };

    recordings::setup(
        main_window.global::<Recordings>(),
        main_window.as_weak(),
        recordings_unfiltered,
        &filter_model,
        &celeste,
    );

    let record_tas_global = main_window.global::<RecordTAS>();
    record_tas_global.on_start_celeste({
        let handle = main_window.as_weak();
        move || {
            if let Err(e) = opener::open_browser("steam://rungameid/504230") {
                handle
                    .unwrap()
                    .set_record_status_text(format!("Could not start celeste: {e:#?}").into());
            }

            let handle = handle.clone();
            std::thread::spawn(move || {
                let attempts_duration = 30 * 1000;
                let attempts_interval = 500;
                let total_attempts = attempts_duration / attempts_interval;

                for i in 0..total_attempts {
                    let (ok, msg, tas_recorder_installed) =
                        record_tas::check_required_mods(&celestedebugrc::DebugRC::new());

                    let time = i * attempts_interval;

                    if !ok {
                        std::thread::sleep(Duration::from_millis(attempts_interval));

                        handle
                            .upgrade_in_event_loop(move |handle| {
                                handle.set_error(format!("Starting celeste... [{}ms]", time).into())
                            })
                            .unwrap();
                    } else {
                        handle
                            .upgrade_in_event_loop(move |handle| {
                                handle.global::<RecordTAS>().set_celeste_started(ok);
                                handle
                                    .global::<RecordTAS>()
                                    .set_tasrecorder_installed(tas_recorder_installed);

                                handle.set_error(msg.unwrap_or_default().into());
                            })
                            .unwrap();
                        break;
                    }
                }
            });
        }
    });

    render::setup(
        main_window.global::<Render>(),
        main_window.as_weak(),
        &filter_model,
        celeste.clone(),
    );

    record_tas::setup(
        main_window.global::<RecordTAS>(),
        main_window.as_weak(),
        celeste,
        physics_inspector,
    );

    main_window.set_recordings(ModelRc::from(filter_model));
    main_window.run().unwrap();
}
