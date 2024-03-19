use std::time::{Duration, Instant};

use anyhow::Result;
use celesteloader::cct_physics_inspector::PhysicsInspector;
use notify_debouncer_full::{
    notify::{self, RecommendedWatcher, Watcher},
    DebounceEventResult, Debouncer, FileIdMap,
};
use slint::Weak;

use crate::MainWindow;

pub fn start_watcher(
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
                    super::read_recordings_update_main(handle, &physics_inspector);

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
