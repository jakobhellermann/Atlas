use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use celestedebugrc::DebugRC;
use celesteloader::cct_physics_inspector::PhysicsInspector;
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};

use crate::{recordings, MainWindow, RecordPath, RecordTAS};

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
                    .map(|file| {
                        let path = file.path();
                        let changed = match is_git_changed(&path) {
                            Ok(rev) => rev,
                            Err(e) => {
                                dbg!(&e);
                                None
                            }
                        };
                        RecordPath {
                            path: file.path().to_str().unwrap().into(),
                            git_commit: changed
                                .map(|(commit, _)| commit)
                                .unwrap_or_default()
                                .into(),
                        }
                    })
                    .collect::<Vec<_>>();
                handle
                    .upgrade_in_event_loop(|handle| {
                        handle.invoke_pick_tas_files_done(Rc::new(VecModel::from(files)).into());
                    })
                    .unwrap();
            });
            runtime.spawn_blocking(move || {
                let ok = debugrc.get("").is_ok();
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
        move |files_model, speedup, run_as_merged, record_git_tree| {
            if let Err(e) = record_tases(
                files_model,
                handle.clone(),
                physics_inspector.clone(),
                debugrc.clone(),
                speedup,
                run_as_merged,
                record_git_tree,
            ) {
                handle.unwrap().set_error(format!("{e}").into());
            }
        }
    });
}

fn record_tases(
    files_model: ModelRc<RecordPath>,
    handle: Weak<MainWindow>,
    physics_inspector: PhysicsInspector,
    debugrc: DebugRC,
    speedup: f32,
    run_as_merged: bool,
    record_git_tree: bool,
) -> Result<()> {
    let mut files = Vec::with_capacity(files_model.row_count());
    let mut tmp_files = Vec::new();
    for file in files_model.iter() {
        let path = PathBuf::from(file.path.to_string());
        if record_git_tree {
            if let Ok(Some((_, data))) = is_git_changed(&path) {
                let tmp = tempfile::Builder::new().suffix(".tas").tempfile()?;
                BufWriter::new(tmp.as_file()).write_all(&data)?;
                files.push(tmp.path().to_owned());
                tmp_files.push(tmp);
            }
        }

        files.push(path);
    }

    std::thread::spawn(move || {
        let _tmp_files = tmp_files;

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
                    let percentage = (status.current_file as f32 + percentage_in_tas)
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

    Ok(())
}

fn is_git_changed(path: &Path) -> Result<Option<(String, Vec<u8>)>> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };

    let Ok(git) = gix::discover(parent) else {
        return Ok(None);
    };

    let relative_path = path
        .strip_prefix(git.work_dir().context("repo has no workdir")?)
        .context("path not in repo??")?;

    let head = git.head_commit()?;
    let tree = head.tree()?;

    let object = tree
        .lookup_entry_by_path(relative_path, &mut Vec::new())?
        .context("path not in repo?")?
        .object()?;

    let data_new = std::fs::read(path)?;
    let changed = object.data != data_new;

    let commit_id = head.short_id()?.to_string();
    Ok(changed.then_some((commit_id, data_new)))
}
