use std::{path::PathBuf, rc::Rc};

use celestedebugrc::DebugRC;
use celesteloader::cct_physics_inspector::PhysicsInspector;
use slint::{ComponentHandle, Model, SharedString, VecModel, Weak};

use crate::{recordings, MainWindow, RecordTAS};

pub fn setup(
    record_tas_global: RecordTAS<'_>,
    main_window: Weak<MainWindow>,
    physics_inspector: PhysicsInspector,
) {
    let debugrc = DebugRC::new();

    let mut runtime = tokio::runtime::Builder::new_multi_thread();
    #[cfg(target_os = "linux")]
    runtime.enable_io();
    let runtime = runtime.build().unwrap();

    record_tas_global.on_pick_tas_files({
        let handle = main_window.clone();
        let debugrc = debugrc.clone();
        move || {
            let debugrc = debugrc.clone();
            let handle = handle.clone();
            let handle_2 = handle.clone();
            runtime.spawn(async move {
                let files = rfd::AsyncFileDialog::new()
                    .add_filter("TAS", &["tas"])
                    .pick_files()
                    .await
                    .unwrap_or_default();
                let files = files
                    .into_iter()
                    .map(|file| file.path().to_str().unwrap().into())
                    .collect::<Vec<SharedString>>();
                handle
                    .upgrade_in_event_loop(|handle| {
                        handle.invoke_pick_tas_files_done(Rc::new(VecModel::from(files)).into());
                    })
                    .unwrap();
            });
            runtime.spawn_blocking(move || {
                let ok = debugrc.get("/").is_ok();
                handle_2
                    .upgrade_in_event_loop(move |handle| {
                        handle.global::<RecordTAS>().set_celeste_started(ok);
                    })
                    .unwrap();
            });
        }
    });

    record_tas_global.on_abort_tas(move || {
        // let _res = DebugRC::new().console("invoke Manager.DisableRun");
        // dbg!(_res);
    });
    record_tas_global.on_record_tases({
        let handle = main_window.clone();
        move |files, speedup, run_as_merged| {
            let files = files
                .iter()
                .map(|p| PathBuf::from(p.to_string()))
                .collect::<Vec<_>>();

            let debugrc = debugrc.clone();

            let handle = handle.clone();
            let physics_inspector = physics_inspector.clone();
            std::thread::spawn(move || {
                let mut last_progress = 0.0;

                let result = debugrc
                    .run_tases_fastforward(&files, speedup, run_as_merged, |status| {
                        let percentage_in_tas = status
                            .current_frame
                            .parse::<u32>()
                            .ok()
                            .and_then(|current| {
                                let total = status.total_frames.parse::<u32>().ok()?;
                                Some((current, total))
                            })
                            .map(|(current, total)| current as f32 / total as f32)
                            .unwrap_or(1.0);

                        let (msg, new_progress) = if let Some(origin) = status.origin {
                            let msg = format!(
                                "{}/{} {origin}: {}/{}",
                                status.current_file,
                                status.total_files,
                                status.current_frame,
                                status.total_frames
                            );
                            let percentage = (status.current_file as f32
                                + percentage_in_tas as f32)
                                / status.total_files as f32;
                            (msg, percentage)
                        } else {
                            let msg = format!("{}/{}", status.current_frame, status.total_frames);
                            (msg, percentage_in_tas)
                        };

                        handle
                            .upgrade_in_event_loop(move |handle| {
                                if new_progress > last_progress {
                                    handle.set_record_progress(new_progress);
                                }
                                handle.set_record_status_text(msg.into());
                            })
                            .unwrap();

                        last_progress = new_progress;
                    })
                    .map(|_| {
                        if let Err(e) = debugrc.get("cct/segmentRecording") {
                            eprintln!("Failed to segment recording: {e}");
                        }
                    });

                handle
                    .upgrade_in_event_loop(move |handle| {
                        match result {
                            Ok(()) => {
                                handle.set_record_status_text("Done!".into());
                                handle.invoke_record_done(true);
                            }
                            Err(err) => {
                                handle.set_record_status_text(format!("{err:?}").into());
                                handle.invoke_record_done(false);
                            }
                        };
                        handle.set_record_progress(1.0);

                        recordings::read_recordings_update_main(handle, &physics_inspector);
                    })
                    .unwrap();
            });
        }
    });
}
