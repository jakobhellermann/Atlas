use anyhow::{Context, Result};
use celestedebugrc::DebugRC;
use celesteloader::{cct_physics_inspector::PhysicsInspector, map::Map, CelesteInstallation};
use celesterender::{
    asset::{AssetDb, ModLookup},
    CelesteRenderData, RenderResult,
};
use chrono::DateTime;
use notify::Watcher;
use slint::{Model, VecModel};
use std::{collections::HashMap, rc::Rc};

slint::include_modules!();

pub fn main() -> Result<()> {
    let celeste = CelesteInstallation::detect()?;
    let physics_inspector = PhysicsInspector::new(&celeste);

    let recordings = Rc::new(slint::VecModel::<CCTRecording>::from(read_recordings(
        &physics_inspector,
    )?));

    let main_window = MainWindow::new().unwrap();
    main_window.set_recordings_model(recordings.clone().into());

    let watcher_handle = main_window.as_weak().clone();

    {
        let recent_recordings_path = physics_inspector.recent_recordings.clone();
        let physics_inspector = physics_inspector.clone();
        let mut watcher = notify::recommended_watcher(move |e| {
            let physics_inspector = &physics_inspector;
            watcher_handle
                .upgrade_in_event_loop(move |handle| {
                    /*let model = handle.get_recordings_model();
                    match read_recordings(&physics_inspector) {
                        Err(e) => handle.set_error(format!("{e:?}").into()),
                        Ok(new) => model
                            .as_any()
                            .downcast_ref::<VecModel<CCTRecording>>()
                            .unwrap()
                            .set_vec(new),
                    }*/
                    let model = handle.get_recordings_model();
                    // read_recordings(physics_inspector);
                })
                .unwrap();
        })?;
        watcher.watch(&recent_recordings_path, notify::RecursiveMode::NonRecursive)?;
    }
    // callbacks
    main_window.on_render({
        let recordings = recordings.clone();
        let handle = main_window.as_weak();
        let mut state = RenderState::new(&celeste)?;

        move || {
            let sids: HashMap<String, Vec<_>> =
                recordings.iter().fold(HashMap::new(), |mut acc, item| {
                    if item.checked {
                        acc.entry(item.sid.into()).or_default().push(item.i);
                    }
                    acc
                });

            let handle = handle.unwrap();
            handle.set_rendering(true);
            render_recordings(sids, &mut state, |e| {
                handle.set_error(format!("{e:?}").into());
            });
            handle.set_rendering(false);
        }
    });

    main_window.on_delete_recent_recordings({
        let physics_inspector = physics_inspector.clone();
        let recordings = Rc::clone(&recordings);
        let handle = main_window.as_weak();
        move || {
            let handle = handle.unwrap();
            if let Err(e) = physics_inspector.delete_recent_recordings() {
                handle.set_error(format!("{e:?}").into());
            }
            match read_recordings(&physics_inspector) {
                Err(e) => handle.set_error(format!("{e:?}").into()),
                Ok(new) => recordings.set_vec(new),
            }
        }
    });
    main_window.on_refresh_recordings({
        let recordings = Rc::clone(&recordings);
        let physics_inspector = physics_inspector.clone();
        let handle = main_window.as_weak();
        move || {
            recordings.set_vec(Vec::new());
            let handle = handle.unwrap();
            match read_recordings(&physics_inspector) {
                Err(e) => handle.set_error(format!("{e:?}").into()),
                Ok(new) => recordings.set_vec(new),
            };
        }
    });
    main_window.on_record_tases({
        let debugrc = DebugRC::new();
        let handle = main_window.as_weak();
        move || {
            let files = native_dialog::FileDialog::new()
                .add_filter("TAS", &["tas"])
                .show_open_multiple_file()
                .unwrap();
            if files.is_empty() {
                return;
            }

            let debugrc = debugrc.clone();

            handle.unwrap().invoke_record_started();

            let handle = handle.clone();
            let physics_inspector = physics_inspector.clone();
            std::thread::spawn(move || {
                let speedup = 4.0;
                let result = debugrc.run_tases_fastforward(&files, speedup, |status| {
                    let percentage = status
                        .current_frame
                        .parse::<u32>()
                        .and_then(|current| {
                            let total = status.total_frames.parse::<u32>()?;
                            Ok((current, total))
                        })
                        .map(|(current, total)| current as f32 / total as f32);

                    let msg = if let Some(origin) = status.origin {
                        format!("{origin}: {}/{}", status.current_frame, status.total_frames)
                    } else {
                        format!("{}/{}", status.current_frame, status.total_frames)
                    };

                    handle
                        .upgrade_in_event_loop(move |handle| {
                            handle.set_record_status(msg.into());
                            if let Ok(percentage) = percentage {
                                handle.set_record_progress(percentage);
                            }
                        })
                        .unwrap();
                });

                handle
                    .upgrade_in_event_loop(move |handle| {
                        match result {
                            Ok(()) => handle.set_record_status("Done!".into()),
                            Err(err) => handle.set_record_status(format!("{err:?}").into()),
                        };

                        handle.invoke_record_done();
                        handle.set_record_progress(1.0);

                        let model = handle.get_recordings_model();
                        match read_recordings(&physics_inspector) {
                            Err(e) => handle.set_error(format!("{e:?}").into()),
                            Ok(new) => model
                                .as_any()
                                .downcast_ref::<VecModel<CCTRecording>>()
                                .unwrap()
                                .set_vec(new),
                        }
                    })
                    .unwrap();
            });
        }
    });

    // run
    main_window.run().unwrap();

    Ok(())
}

struct RenderState {
    celeste: CelesteInstallation,
    physics_inspector: PhysicsInspector,
    asset_db: AssetDb<ModLookup>,
    render_data: CelesteRenderData,
}
impl RenderState {
    fn new(celeste: &CelesteInstallation) -> Result<Self> {
        Ok(RenderState {
            celeste: celeste.clone(),
            physics_inspector: PhysicsInspector::new(celeste),
            asset_db: AssetDb::new(ModLookup::all_mods(celeste)?),
            render_data: CelesteRenderData::base(&celeste)?,
        })
    }

    fn render(&mut self, sid: &str) -> Result<(RenderResult, Map)> {
        let (result, map) = celesterender::render_map_sid(
            &self.celeste,
            &mut self.render_data,
            &mut self.asset_db,
            sid,
        )
        .with_context(|| format!("{sid}"))?;

        Ok((result, map))
    }
}

fn render_recordings(
    sids: HashMap<String, Vec<i32>>,
    state: &mut RenderState,
    on_error: impl Fn(anyhow::Error),
) {
    for (sid, recordings) in sids {
        if let Err(e) = (|| -> Result<()> {
            let a = std::time::Instant::now();
            let (mut result, map) = state.render(&sid)?;

            let size_filled = map.rooms.iter().map(|room| room.bounds.area()).sum::<f32>();
            let size = result.bounds.area();
            let density = size_filled / size;

            for recording in recordings {
                let width = Some(2.0);
                let width = width.unwrap_or_else(|| if density > 0.5 { 8.0 } else { 3.0 });

                annotate_celeste_map::annotate_cct_recording_skia(
                    &mut result.image,
                    &state.physics_inspector,
                    recording as u32,
                    result.bounds,
                    width,
                )?;
            }

            let tmp = std::env::temp_dir();
            let out_path = tmp.join(format!("{}.png", sid.replace(['/'], "_")));
            result.image.save_png(&out_path)?;

            println!("Rendered map {sid} in {:.2}ms", a.elapsed().as_millis());

            opener::open(&out_path)?;
            Ok(())
        })() {
            eprintln!("{e:?}");
            on_error(e);
        }
    }
}

fn read_recordings(physics_inspector: &PhysicsInspector) -> Result<Vec<CCTRecording>> {
    let mut recent_recordings = physics_inspector.recent_recordings()?;
    recent_recordings.sort_by_key(|a| a.0);

    Ok(recent_recordings
        .into_iter()
        .map(|(i, layout)| {
            let name = if layout.side_name == "A-Side" {
                layout.chapter_name
            } else {
                format!("{} {}", layout.chapter_name, layout.side_name)
            };
            CCTRecording {
                i: i as i32,
                chapter_name: name.into(),
                sid: layout.sid.as_deref().unwrap_or_default().into(),
                start_time: DateTime::parse_from_rfc3339(&layout.recording_started)
                    .unwrap()
                    .format("%d.%m.%Y %R")
                    .to_string()
                    .into(),
                can_render: layout.sid.is_some(),
                checked: true,
            }
        })
        .collect())
}
