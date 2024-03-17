#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use annotate_celeste_map::{ColorMode, LineSettings};
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
    panic::AssertUnwindSafe,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

slint::include_modules!();

pub fn main() -> Result<()> {
    let celeste = CelesteInstallation::detect()?;
    let physics_inspector = PhysicsInspector::new(&celeste);

    let main_window = MainWindow::new().unwrap();
    let _watcher = start_watcher(&physics_inspector, main_window.as_weak())?;

    let recordings = Rc::new(VecModel::from(read_recordings(&physics_inspector)?));

    main_window.global::<Recordings>().on_toggle_expand_map({
        let recordings = recordings.clone();
        move |map_bin| {
            let Some(list) = recordings
                .iter()
                .find(|recording| recording.map_bin == map_bin)
            else {
                return;
            };

            for i in 0..list.recordings.row_count() {
                let mut row = list.recordings.row_data(i).unwrap();
                row.checked = list.checked;
                list.recordings.set_row_data(i, row);
            }
        }
    });
    main_window
        .global::<Recordings>()
        .on_toggle_expand_map_recording({
            let recordings = recordings.clone();
            move |map_bin, _| {
                let Some((j, mut list)) = recordings
                    .iter()
                    .enumerate()
                    .find(|(_, recording)| recording.map_bin == map_bin)
                else {
                    return;
                };

                let any_checked = list.recordings.iter().any(|rec| rec.checked);
                list.checked = any_checked;
                recordings.set_row_data(j, list);
            }
        });
    main_window.set_recordings(recordings.clone().into());

    // callbacks
    main_window.on_render({
        let recordings = recordings.clone();
        let handle = main_window.as_weak();

        move |settings| {
            let color_mode = match settings.color_mode.as_str() {
                "Gradient" => ColorMode::Gradient,
                "StState" => ColorMode::State,
                "Red" => ColorMode::Color([255, 0, 0, 255]),
                _ => unreachable!(),
            };

            let map_bins: IndexMap<(String, String), Vec<_>> = recordings
                .iter()
                .filter_map(|map| {
                    let key = (map.map_bin.to_string(), map.chapter_name.to_string());
                    let recordings: Vec<_> = map
                        .recordings
                        .iter()
                        .filter_map(|rec| rec.checked.then_some(rec.i))
                        .collect();
                    (!recordings.is_empty()).then_some((key, recordings))
                })
                .collect();

            if map_bins.is_empty() {
                handle.unwrap().set_error("No recordings selected".into());
                return;
            }

            handle.unwrap().set_rendering(true);
            let celeste = celeste.clone();
            let handle = handle.clone();
            std::thread::spawn(move || {
                let result = render_recordings(
                    map_bins,
                    &celeste,
                    LineSettings {
                        width: settings.width,
                        color_mode,
                        anti_alias: settings.anti_alias,
                    },
                    settings.only_render_visited,
                    |e| {
                        let msg = format!("{e:?}").into();
                        handle
                            .upgrade_in_event_loop(move |handle| handle.set_error(msg))
                            .unwrap();
                    },
                );
                handle
                    .upgrade_in_event_loop(|handle| {
                        handle.set_rendering(false);

                        if let Err(e) = result {
                            handle.set_error(format!("{e:?}").into());
                        }
                    })
                    .unwrap();
            });
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
        let recordings = recordings.clone();
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
    main_window.on_pick_tas_files({
        let handle = main_window.as_weak();
        move || {
            let handle = handle.clone();

            std::thread::spawn(move || {
                let files = rfd::FileDialog::new()
                    .add_filter("TAS", &["tas"])
                    .pick_files()
                    .unwrap_or_default();
                let files = files
                    .into_iter()
                    .map(|file| file.to_str().unwrap().into())
                    .collect::<Vec<SharedString>>();
                handle
                    .upgrade_in_event_loop(|handle| {
                        handle.invoke_pick_tas_files_done(Rc::new(VecModel::from(files)).into());
                    })
                    .unwrap();
            });
        }
    });

    main_window.on_abort_tas(move || {
        // let _res = DebugRC::new().console("invoke Manager.DisableRun");
        // dbg!(_res);
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

                        let model = handle.get_recordings();
                        match read_recordings(&physics_inspector) {
                            Err(e) => handle.set_error(format!("{e:?}").into()),
                            Ok(new) => model
                                .as_any()
                                .downcast_ref::<VecModel<MapRecordings>>()
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
            render_data: CelesteRenderData::base(celeste)?,
        })
    }

    fn render(
        &mut self,
        map_bin: &str,
        settings: RenderMapSettings,
    ) -> Result<(RenderResult, Map)> {
        catch(|| {
            celesterender::render_map_bin(
                &self.celeste,
                &mut self.render_data,
                &mut self.asset_db,
                map_bin,
                settings,
            )
        })
    }
}

fn render_recordings(
    map_bins: IndexMap<(String, String), Vec<i32>>,
    celeste: &CelesteInstallation,
    line_settings: LineSettings,
    mut only_include_visited_rooms: bool,
    on_error: impl Fn(anyhow::Error),
) -> Result<()> {
    let mut state = RenderState::new(&celeste)?;

    for ((map_bin, name), recordings) in map_bins.into_iter().rev() {
        let visited_rooms = if only_include_visited_rooms {
            cct_visited_rooms(&recordings, &state.physics_inspector).unwrap_or_else(|e| {
                eprintln!("Couldn't read room layouts, falling back to including all rooms: {e}");
                only_include_visited_rooms = false;
                Default::default()
            })
        } else {
            HashSet::new()
        };

        if let Err(e) = (|| -> Result<()> {
            let start = std::time::Instant::now();
            let render_settings = RenderMapSettings {
                layer: Layer::ALL,
                include_room: &|room| {
                    !only_include_visited_rooms
                        || visited_rooms.contains(room.name.trim_start_matches("lvl_"))
                },
            };
            let (mut result, _map) = state
                .render(&map_bin, render_settings)
                .with_context(|| format!("failed to render {name}"))?;

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
                    line_settings,
                )?;
            }

            let tmp = std::env::temp_dir();
            let out_path = tmp.join(format!("{}.png", map_bin.replace(['/'], "_")));
            result.image.save_png(&out_path)?;

            println!(
                "Rendered map {map_bin} in {:.2}ms",
                start.elapsed().as_millis()
            );

            opener::open(&out_path)?;
            Ok(())
        })() {
            eprintln!("{e:?}");
            on_error(e);
        }
    }

    Ok(())
}

fn cct_visited_rooms(
    recordings: &[i32],
    physics_inspector: &PhysicsInspector,
) -> Result<HashSet<String>> {
    let mut rooms = HashSet::new();
    for &recording in recordings {
        let layout = physics_inspector.room_layout(recording as u32)?;
        rooms.extend(layout.rooms.into_iter().map(|room| room.debug_room_name));
    }
    Ok(rooms)
}

fn read_recordings(physics_inspector: &PhysicsInspector) -> Result<Vec<MapRecordings>> {
    let mut recent_recordings = physics_inspector.recent_recordings()?;
    recent_recordings.sort_by_key(|a| a.0);

    let now = chrono::Utc::now();

    let mut recordings = IndexMap::<_, Vec<_>>::new();
    for (i, layout) in recent_recordings {
        let is_vanilla = layout.sid.map_or(false, |sid| sid.starts_with("Celeste/"));
        let map_bin = layout.map_bin.unwrap_or_default();
        let map_bin = match is_vanilla {
            true => format!("Celeste/{map_bin}"),
            false => map_bin.into(),
        };

        let name = match layout.side_name.as_str() {
            "A-Side" => layout.chapter_name,
            _ => format!("{} {}", layout.chapter_name, layout.side_name),
        };

        let start_time = DateTime::parse_from_rfc3339(&layout.recording_started)
            .map(|date| {
                let is_today = date.date_naive() == now.date_naive();
                if is_today {
                    date.format("%R").to_string()
                } else {
                    date.format("%d.%m.%Y %R").to_string()
                }
            })
            .unwrap_or_default();

        let start_room = layout
            .rooms
            .first()
            .map(|room| room.debug_room_name.as_str())
            .unwrap_or_default();

        recordings
            .entry((map_bin, name))
            .or_default()
            .push(Recording {
                checked: false,
                i: i as i32,
                start_time: start_time.into(),
                start_room: start_room.into(),
                frame_count: layout.frame_count as i32,
            });
    }

    Ok(recordings
        .into_iter()
        .map(|((map_bin, name), recordings)| MapRecordings {
            map_bin: map_bin.into(),
            chapter_name: name.into(),
            checked: false,
            recordings: Rc::new(VecModel::from(recordings)).into(),
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
                    let model = handle.get_recordings();
                    match read_recordings(&physics_inspector) {
                        Err(e) => {
                            eprintln!("{:?}", e.context("failed to reload recordings"));
                        }
                        Ok(new) => model
                            .as_any()
                            .downcast_ref::<VecModel<MapRecordings>>()
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

fn catch<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| f())) {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(val)) => Err(val),
        Err(e) => {
            if let Some(s) = e.downcast_ref::<String>() {
                Err(anyhow::anyhow!("panic: {s}"))
            } else if let Some(s) = e.downcast_ref::<&str>() {
                Err(anyhow::anyhow!("panic: {s}"))
            } else {
                Err(anyhow::anyhow!("panicked"))
            }
        }
    }
}
