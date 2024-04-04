use anyhow::Result;
use celesteloader::{cct_physics_inspector::PhysicsInspector, CelesteInstallation};
use chrono::DateTime;
use copypasta::ClipboardProvider;
use indexmap::IndexMap;
use slint::{ComponentHandle, FilterModel, Model, VecModel, Weak};
use std::rc::Rc;

mod filtered_recordings;
pub mod watcher;

use crate::{MainWindow, MapRecordings, Recording, Recordings};

#[allow(clippy::type_complexity)]
pub fn load_model(
    main_window: &MainWindow,
    physics_inspector: &PhysicsInspector,
) -> (
    Rc<VecModel<MapRecordings>>,
    Rc<FilterModel<Rc<VecModel<MapRecordings>>, impl Fn(&MapRecordings) -> bool>>,
) {
    let result = match read_recordings(physics_inspector) {
        Ok(recordings) => recordings,
        Err(e) => {
            main_window.set_error(format!("{e:?}").into());
            Vec::new()
        }
    };

    let recordings_unfiltered = Rc::new(VecModel::from(result));
    let filter_model = Rc::new(filtered_recordings::create_model(
        recordings_unfiltered.clone(),
    ));

    (recordings_unfiltered, filter_model)
}

pub fn setup(
    recordings_global: Recordings<'_>,
    main_window: Weak<MainWindow>,
    recordings_unfiltered: Rc<VecModel<MapRecordings>>,
    filter_model: &Rc<FilterModel<Rc<VecModel<MapRecordings>>, impl Fn(&MapRecordings) -> bool>>,
    celeste: &CelesteInstallation,
) {
    recordings_global.on_select_all({
        let handle = main_window.clone();
        let recordings = recordings_unfiltered.clone();
        move || {
            let all_selected = recordings
                .iter()
                .all(|map| map.checked || map.map_bin.is_empty());
            let new_selection = !all_selected;

            let mut new = Vec::new();
            for j in 0..recordings.row_count() {
                let mut map = recordings.row_data(j).unwrap();
                let has_map_bin = !map.map_bin.is_empty();

                for i in 0..map.recordings.row_count() {
                    let mut recording = map.recordings.row_data(i).unwrap();
                    recording.checked = new_selection && has_map_bin;
                    map.recordings.set_row_data(i, recording);
                }

                map.checked = new_selection && has_map_bin;
                new.push(map);
            }
            recordings.set_vec(new);

            recalc_compare_recordings_enabled(handle.clone(), &recordings);
        }
    });
    recordings_global.on_toggle_map({
        let handle = main_window.clone();
        let recordings = recordings_unfiltered.clone();
        move |map_bin| {
            let Some(list) = recordings
                .iter()
                .find(|recording| recording.map_bin == map_bin)
            else {
                return;
            };

            for i in 0..list.recordings.row_count() {
                let mut recording = list.recordings.row_data(i).unwrap();
                recording.checked = list.checked;
                list.recordings.set_row_data(i, recording);
            }

            recalc_compare_recordings_enabled(handle.clone(), &recordings);
        }
    });
    recordings_global.on_toggle_map_recording({
        let handle = main_window.clone();
        let recordings = recordings_unfiltered.clone();
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

            recalc_compare_recordings_enabled(handle.clone(), &recordings);
        }
    });

    recordings_global.on_compare_times({
        let recordings = recordings_unfiltered.clone();
        let celeste = celeste.clone();
        let handle = main_window.clone();
        move || {
            compare_recordings(handle.clone(), &recordings, &celeste);
        }
    });
    recordings_global.on_refresh_recordings({
        let recordings = recordings_unfiltered.clone();
        let physics_inspector = celeste.physics_inspector();
        let handle = main_window.clone();
        move || {
            recordings.set_vec(Vec::new());
            let handle = handle.unwrap();

            match read_recordings(&physics_inspector) {
                Err(e) => handle.set_error(format!("{e:?}").into()),
                Ok(new) => recordings.set_vec(new),
            };
        }
    });
    recordings_global.on_delete_recordings({
        let physics_inspector = celeste.physics_inspector();
        let recordings = Rc::clone(&recordings_unfiltered);
        let handle = main_window.clone();
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
    recordings_global.on_set_filter({
        let filter_model = filter_model.clone();
        move |filter| filtered_recordings::set_filter(&filter, &*filter_model)
    });
}

pub fn read_recordings_update_main(handle: MainWindow, physics_inspector: &PhysicsInspector) {
    let model = handle.get_recordings();
    let model = filtered_recordings::get_source_vec_model(&model);
    match read_recordings(physics_inspector) {
        Err(e) => handle.set_error(format!("{e:?}").into()),
        Ok(new) => model.set_vec(new),
    }
}

pub fn read_recordings(physics_inspector: &PhysicsInspector) -> Result<Vec<MapRecordings>> {
    let mut recent_recordings = physics_inspector.recent_recordings()?;
    recent_recordings.sort_by_key(|a| a.0);

    let now = chrono::Utc::now();

    let mut recordings = IndexMap::<_, Vec<_>>::new();
    for (i, layout) in recent_recordings {
        let old_cct = layout.map_bin.is_none();

        let is_vanilla = layout.sid.map_or(false, |sid| sid.starts_with("Celeste/"));
        let map_bin = layout.map_bin.unwrap_or_default();
        let map_bin = match is_vanilla && !old_cct {
            true => format!("Celeste/{map_bin}"),
            false => map_bin,
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

fn recalc_compare_recordings_enabled(
    handle: Weak<MainWindow>,
    recordings: &VecModel<MapRecordings>,
) {
    let main_window = handle.unwrap();

    let mut all_exactly_two = true;
    let mut any_two = false;

    for map in recordings.iter() {
        let n_checked = map.recordings.iter().filter(|rec| rec.checked).count();

        if n_checked == 0 {
            continue;
        }

        if n_checked == 2 {
            any_two = true;
        } else {
            all_exactly_two = false;
        }
    }

    main_window
        .global::<Recordings>()
        .set_compare_recordings_enabled(any_two && all_exactly_two);
}

fn compare_recordings(
    handle: Weak<MainWindow>,
    recordings_unfiltered: &VecModel<MapRecordings>,
    celeste: &CelesteInstallation,
) {
    let mut maps: IndexMap<String, Vec<u32>> = IndexMap::new();
    for map in recordings_unfiltered.iter() {
        if map.chapter_name.is_empty() {
            continue;
        };

        for rec in map.recordings.iter() {
            if rec.checked {
                maps.entry(map.map_bin.to_string())
                    .or_default()
                    .push(rec.i as u32);
            }
        }
    }

    let celeste = celeste.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<_> {
            let mut renders = Vec::new();

            for (map_bin, recordings) in maps {
                let (map, archive) = celeste.find_map_by_map_bin(&map_bin)?;

                let map_name = archive
                    .map(|mut archive| -> Result<_> {
                        let dialog = archive.get_dialog("English")?;
                        let map_name = dialog.get(&map_bin).unwrap_or(&map_bin);
                        Ok(map_name.to_owned())
                    })
                    .transpose()?
                    .unwrap_or_else(|| map_bin.clone());

                if recordings.len() != 2 {
                    continue;
                }

                let a = celesteloader::cct_physics_inspector::compare_timesave::compare_timesave(
                    &celeste.physics_inspector(),
                    &map,
                    &map_name,
                    (recordings[0], recordings[1]),
                )?;
                renders.push(a);
            }

            Ok(renders)
        })();

        handle
            .upgrade_in_event_loop(move |handle| match result {
                Ok(renderings) => {
                    let text = renderings.join("\n\n");

                    let clip_msg = match copypasta::ClipboardContext::new()
                        .and_then(|mut clip| clip.set_contents(text.clone()))
                    {
                        Ok(()) => "Copied to clipboard".into(),
                        Err(e) => format!("Failed to copy to clipboard: {e}"),
                    };

                    handle.set_compare_timesave_text(format!("{text}\n{clip_msg}").into());
                }
                Err(e) => {
                    handle.set_error(format!("{e:?}").into());
                }
            })
            .unwrap();
    });
}
