use anyhow::Result;
use celesteloader::cct_physics_inspector::PhysicsInspector;
use chrono::DateTime;
use indexmap::IndexMap;
use slint::{FilterModel, Model, VecModel, Weak};
use std::rc::Rc;

mod filtered_recordings;
pub mod watcher;

use crate::{MainWindow, MapRecordings, Recording, Recordings};

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
    physics_inspector: &PhysicsInspector,
    filter_model: &Rc<FilterModel<Rc<VecModel<MapRecordings>>, impl Fn(&MapRecordings) -> bool>>,
) {
    recordings_global.on_toggle_select_all({
        let recordings = recordings_unfiltered.clone();
        move || {
            let all_selected = recordings
                .iter()
                .all(|map| map.checked || map.map_bin == "");
            let new_selection = if all_selected { false } else { true };

            let mut new = Vec::new();
            for j in 0..recordings.row_count() {
                let mut map = recordings.row_data(j).unwrap();
                let has_map_bin = map.map_bin != "";

                for i in 0..map.recordings.row_count() {
                    let mut recording = map.recordings.row_data(i).unwrap();
                    recording.checked = new_selection && has_map_bin;
                    map.recordings.set_row_data(i, recording);
                }

                map.checked = new_selection && has_map_bin;
                new.push(map);
            }
            recordings.set_vec(new);
        }
    });
    recordings_global.on_toggle_expand_map({
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
        }
    });
    recordings_global.on_toggle_expand_map_recording({
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
        }
    });
    recordings_global.on_refresh_recordings({
        let recordings = recordings_unfiltered.clone();
        let physics_inspector = physics_inspector.clone();
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
        let physics_inspector = physics_inspector.clone();
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
