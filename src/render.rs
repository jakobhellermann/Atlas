use annotate_celeste_map::{ColorMode, LineSettings};
use anyhow::{Context, Result};
use celesteloader::{cct_physics_inspector::PhysicsInspector, map::Map, CelesteInstallation};
use celesterender::asset::{AssetDb, ModLookup};
use celesterender::{CelesteRenderData, Layer, RenderMapSettings, RenderResult};
use indexmap::IndexMap;
use slint::{ComponentHandle, FilterModel, Model, VecModel, Weak};
use std::{collections::HashSet, panic::AssertUnwindSafe, rc::Rc};

use crate::{MainWindow, MapRecordings, Render};

pub fn setup(
    render_global: Render<'_>,
    main_window: Weak<MainWindow>,
    filter_model: &Rc<FilterModel<Rc<VecModel<MapRecordings>>, impl Fn(&MapRecordings) -> bool>>,
    celeste: CelesteInstallation,
) {
    render_global.on_render({
        let recordings = filter_model.clone();
        let handle = main_window.clone();

        move |settings| {
            let color_mode = match settings.color_mode.as_str() {
                "Gradient" => ColorMode::Gradient,
                "Random" => ColorMode::Random,
                "StState" => ColorMode::State,
                "Red" => ColorMode::Color([255, 0, 0, 255]),
                _ => unreachable!(),
            };

            let layer = [
                (settings.layer.fgtiles, Layer::TILES_FG),
                (settings.layer.bgtiles, Layer::TILES_BG),
                (settings.layer.entities, Layer::ENTITIES),
                (settings.layer.fgdecals, Layer::DECALS_FG),
                (settings.layer.bgdecals, Layer::DECALS_BG),
            ]
            .into_iter()
            .fold(
                Layer::NONE,
                |acc, (include, layer)| {
                    if include {
                        acc | layer
                    } else {
                        acc
                    }
                },
            );

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

            handle.unwrap().global::<Render>().set_rendering(true);
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
                    layer,
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
                        handle.global::<Render>().set_rendering(false);

                        if let Err(e) = result {
                            handle.set_error(format!("{e:?}").into());
                        }
                    })
                    .unwrap();
            });
        }
    });
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
    layer: Layer,
    mut only_include_visited_rooms: bool,
    on_error: impl Fn(anyhow::Error),
) -> Result<()> {
    let mut state = RenderState::new(celeste)?;

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
                layer,
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

            // let width = Some(2.0);
            // let width = width.unwrap_or_else(|| if density > 0.5 { 8.0 } else { 3.0 });

            annotate_celeste_map::annotate_cct_recording_skia(
                &mut result.image,
                &state.physics_inspector,
                recordings.into_iter().map(|i| i as u32),
                result.bounds,
                line_settings,
            )?;

            let tmp = std::env::temp_dir().join("atlas");
            std::fs::create_dir_all(&tmp)?;
            let out_path = tmp.join(format!("{}.png", map_bin.replace(['/'], "_")));
            result.image.save_png(&out_path)?;

            println!(
                "Rendered map {map_bin} in {:.2}ms",
                start.elapsed().as_millis()
            );

            if result.unknown_entities.len() > 0 {
                let mut unknown = result.unknown_entities.iter().collect::<Vec<_>>();
                unknown.sort_by_key(|&(_, n)| std::cmp::Reverse(n));

                eprintln!(
                    "Found {:2} unknown entities: ({} ...)",
                    unknown.len(),
                    unknown
                        .iter()
                        .take(5)
                        .map(|(name, num)| format!("{num} {name} "))
                        .collect::<String>()
                );
            }

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

fn catch<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
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
