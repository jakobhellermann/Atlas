#![windows_subsystem = "windows"]

use anyhow::{Context, Result};
use celestedebugrc::DebugRC;
use celesteloader::{cct_physics_inspector::PhysicsInspector, map::Map, CelesteInstallation};
use celesterender::{
    asset::{AssetDb, ModLookup},
    CelesteRenderData, Layer, RenderMapSettings, RenderResult,
};
use chrono::DateTime;
use indexmap::IndexMap;
use notify_debouncer_full::{
    notify::{self, RecommendedWatcher, Watcher},
    DebounceEventResult, Debouncer, FileIdMap,
};
use slint::{Model, SharedString, VecModel, Weak};
use std::{
    collections::HashSet,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

slint::include_modules!();

pub fn main() -> Result<()> {
    let celeste = CelesteInstallation::detect()?;
    let physics_inspector = PhysicsInspector::new(&celeste);

    let recordings = Rc::new(slint::VecModel::<CCTRecording>::from(read_recordings(
        &physics_inspector,
    )?));

    let main_window = MainWindow::new().unwrap();
    main_window.set_recordings_model(recordings.clone().into());

    let _watcher = start_watcher(&physics_inspector, main_window.as_weak())?;

    // callbacks
    main_window.on_render({
        let recordings = recordings.clone();
        let handle = main_window.as_weak();
        let mut state = RenderState::new(&celeste)?;

        move |width, only_include_visited_rooms| {
            let sids: IndexMap<String, Vec<_>> =
                recordings.iter().fold(IndexMap::new(), |mut acc, item| {
                    if item.checked {
                        acc.entry(item.sid.into()).or_default().push(item.i);
                    }
                    acc
                });

            let handle = handle.unwrap();
            handle.set_rendering(true);
            render_recordings(sids, &mut state, width, only_include_visited_rooms, |e| {
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
    main_window.on_pick_tas_files(move || {
        let files = native_dialog::FileDialog::new()
            .add_filter("TAS", &["tas"])
            .show_open_multiple_file()
            .unwrap();
        let files = files
            .into_iter()
            .map(|file| file.to_str().unwrap().into())
            .collect::<Vec<SharedString>>();
        Rc::new(VecModel::from(files)).into()
    });

    main_window.on_abort_tas(move || {
        let _res = DebugRC::new().send_tas_keybind("Start");
    });
    main_window.on_record_tases({
        let debugrc = DebugRC::new();
        let handle = main_window.as_weak();
        move |files, speedup, run_as_merged| {
            let files = files
                .iter()
                .map(|p| PathBuf::from(p.to_string()))
                .collect::<Vec<_>>();

            let debugrc = debugrc.clone();

            let handle = handle.clone();
            let physics_inspector = physics_inspector.clone();
            std::thread::spawn(move || {
                let result =
                    debugrc.run_tases_fastforward(&files, speedup, run_as_merged, |status| {
                        let (msg, percentage) = if let Some(origin) = status.origin {
                            let msg = format!(
                                "{origin} {}/{}: {}/{}",
                                status.current_file,
                                status.total_files,
                                status.current_frame,
                                status.total_frames
                            );
                            let percentage = status.current_file as f32 / status.total_files as f32;
                            (msg, percentage)
                        } else {
                            let msg = format!("{}/{}", status.current_frame, status.total_frames);

                            let percentage = status
                                .current_frame
                                .parse::<u32>()
                                .ok()
                                .and_then(|current| {
                                    let total = status.total_frames.parse::<u32>().ok()?;
                                    Some((current, total))
                                })
                                .map(|(current, total)| current as f32 / total as f32)
                                .unwrap_or(1.0);

                            (msg, percentage)
                        };

                        handle
                            .upgrade_in_event_loop(move |handle| {
                                handle.set_record_status_text(msg.into());
                                handle.set_record_progress(percentage);
                            })
                            .unwrap();
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

    fn render(&mut self, sid: &str, settings: RenderMapSettings) -> Result<(RenderResult, Map)> {
        let (result, map) = celesterender::render_map_sid(
            &self.celeste,
            &mut self.render_data,
            &mut self.asset_db,
            sid,
            settings,
        )
        .with_context(|| format!("{sid}"))?;

        Ok((result, map))
    }
}

fn render_recordings(
    sids: IndexMap<String, Vec<i32>>,
    state: &mut RenderState,
    width: f32,
    mut only_include_visited_rooms: bool,
    on_error: impl Fn(anyhow::Error),
) {
    for (sid, recordings) in sids.into_iter().rev() {
        let visited_rooms = if only_include_visited_rooms {
            let mut rooms = HashSet::new();
            for &recording in &recordings {
                let layout = match state.physics_inspector.room_layout(recording as u32) {
                    Ok(layout) => layout,
                    Err(e) => {
                        eprintln!(
                            "Couldn't read room layouts, falling back to including all rooms: {e}"
                        );
                        only_include_visited_rooms = false;
                        break;
                    }
                };
                rooms.extend(layout.rooms.into_iter().map(|room| room.debug_room_name));
            }
            rooms
        } else {
            Default::default()
        };

        if let Err(e) = (|| -> Result<()> {
            let a = std::time::Instant::now();
            let (mut result, _map) = state.render(
                &sid,
                RenderMapSettings {
                    layer: Layer::ALL,
                    include_room: &|room| {
                        !only_include_visited_rooms
                            || visited_rooms.contains(room.name.trim_start_matches("lvl_"))
                    },
                },
            )?;

            // let size_filled = map.rooms.iter().map(|room| room.bounds.area()).sum::<f32>();
            // let size = result.bounds.area();
            // let density = size_filled / size;

            for recording in recordings {
                // let width = Some(2.0);
                // let width = width.unwrap_or_else(|| if density > 0.5 { 8.0 } else { 3.0 });

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

fn start_watcher(
    physics_inspector: &PhysicsInspector,
    watcher_handle: Weak<MainWindow>,
) -> Result<Debouncer<RecommendedWatcher, FileIdMap>> {
    let recent_recordings_path = physics_inspector.recent_recordings.clone();
    let physics_inspector = physics_inspector.clone();

    let mut last_event = Instant::now();

    let mut debouncer = notify_debouncer_full::new_debouncer(
        Duration::from_millis(200),
        None,
        move |event: DebounceEventResult| {
            let Ok(event) = event else { return };

            let room_layout_changed =
                event
                    .iter()
                    .flat_map(|event| &event.event.paths)
                    .any(|path| {
                        path.to_str()
                            .map_or(false, |e| e.ends_with("_room-layout.json"))
                    });

            if room_layout_changed {
                /*for event in &event {
                    for path in &event.event.paths {
                        println!("{:?} {}", event.kind, path.display());
                    }
                }*/
                let now = Instant::now();
                let since_last = now.duration_since(last_event);
                last_event = now;

                let physics_inspector = physics_inspector.clone();
                let result = watcher_handle.upgrade_in_event_loop(move |handle| {
                    let start_reading = Instant::now();
                    let model = handle.get_recordings_model();
                    match read_recordings(&physics_inspector) {
                        Err(e) => handle.set_error(format!("{e:?}").into()),
                        Ok(new) => model
                            .as_any()
                            .downcast_ref::<VecModel<CCTRecording>>()
                            .unwrap()
                            .set_vec(new),
                    }

                    println!(
                        "reloading room layouts, {:.02}s after last, took {}ms",
                        since_last.as_secs_f32(),
                        start_reading.elapsed().as_secs_f32() / 1000.,
                    );
                });

                if let Err(e) = result {
                    eprintln!("failed to reload room layouts: {e}");
                }
            }
        },
    )?;
    debouncer
        .watcher()
        .watch(&recent_recordings_path, notify::RecursiveMode::NonRecursive)?;
    debouncer
        .cache()
        .add_root(&recent_recordings_path, notify::RecursiveMode::NonRecursive);

    Ok(debouncer)
}
