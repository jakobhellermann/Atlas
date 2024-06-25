use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{Context, Result};
use celestedebugrc::DebugRC;
use celesteloader::cct_physics_inspector::PhysicsInspector;
use celesteloader::CelesteInstallation;
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};

use crate::{recordings, MainWindow, RecordPath, RecordTAS, RecordTasSettings};

pub fn setup(
    record_tas_global: RecordTAS<'_>,
    main_window: Weak<MainWindow>,
    celeste: CelesteInstallation,
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
                        let git_commit = match is_git_changed(&path) {
                            Ok(Some((commit, _))) => commit,
                            Ok(None) => String::new(),
                            Err(e) => {
                                eprintln!("{}", e);
                                String::new()
                            }
                        };
                        RecordPath {
                            path: file.path().to_str().unwrap().into(),
                            git_commit: git_commit.into(),
                        }
                    })
                    .collect::<Vec<_>>();
                handle
                    .upgrade_in_event_loop(|handle| {
                        let any_changed = files.iter().any(|file| !file.git_commit.is_empty());
                        handle.invoke_pick_tas_files_done(
                            Rc::new(VecModel::from(files)).into(),
                            any_changed,
                        );
                    })
                    .unwrap();
            });

            runtime.spawn_blocking(move || {
                let (ok, msg, tas_recorder_installed) = check_required_mods(&debugrc);
                handle_2
                    .upgrade_in_event_loop(move |handle| {
                        handle.global::<RecordTAS>().set_celeste_started(ok);
                        handle
                            .global::<RecordTAS>()
                            .set_tasrecorder_installed(tas_recorder_installed);
                        if let Some(msg) = msg {
                            handle.set_error(msg.into());
                        }
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
        let celeste = celeste.clone();
        move |files_model, settings| {
            if let Err(e) = record_tases(
                files_model,
                handle.clone(),
                celeste.clone(),
                physics_inspector.clone(),
                debugrc.clone(),
                settings,
            ) {
                handle.unwrap().set_error(format!("{e}").into());
            }
        }
    });
}

fn record_tases(
    files_model: ModelRc<RecordPath>,
    handle: Weak<MainWindow>,
    celeste: CelesteInstallation,
    physics_inspector: PhysicsInspector,
    debugrc: DebugRC,
    settings: RecordTasSettings,
) -> Result<()> {
    let mut files = Vec::with_capacity(files_model.row_count());
    let mut tmp_files = Vec::new();

    let enable_base = "Set,ConsistencyTracker.LogPhysicsEnabled,true";
    let disable_base = "Set,ConsistencyTracker.LogPhysicsEnabled,false";

    let record_ghost = settings.record_git_tree && settings.enable_tas_recorder;

    let decorate = {
        let mut decorate_begin = enable_base.to_owned();
        let mut decorate_end = disable_base.to_owned();
        if settings.enable_tas_recorder {
            decorate_begin.push('\n');
            decorate_begin.push_str("StartRecording");
            decorate_end.push('\n');
            decorate_end.push_str("StopRecording");
        }
        if record_ghost {
            decorate_begin.push('\n');
            decorate_begin.push_str("StartGhostReplay");
        }
        (decorate_begin, decorate_end)
    };

    let decorate_orig = {
        let mut decorate_orig_begin = enable_base.to_owned();
        let mut decorate_orig_end = disable_base.to_owned();
        if record_ghost {
            decorate_orig_begin.push_str("\nStartGhostRecording");
            decorate_orig_end.push_str("\nStopGhostRecording");
        }
        (decorate_orig_begin, decorate_orig_end)
    };

    for file in files_model.iter() {
        let path = PathBuf::from(file.path.to_string());
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();

        let parent = path.parent().unwrap_or(Path::new("/"));

        if settings.only_record_changes {
            let old_new = with_old_new(&path, |_, old, new| (old.to_owned(), new))?;
            match old_new {
                Some((old, new)) => {
                    if settings.record_git_tree {
                        let only_diff_reverse =
                            physics_log_in_diff(&new, &old, decorate_orig.clone());
                        let tmpfile = write_to_temp_in(&only_diff_reverse, parent, &mut tmp_files)?;

                        files.push((tmpfile, format!("{name} original"), decorate_orig.clone()));
                    }

                    let only_diff = physics_log_in_diff(&old, &new, decorate.clone());
                    let tmpfile = write_to_temp_in(&only_diff, parent, &mut tmp_files)?;
                    files.push((tmpfile, name, decorate.clone()));
                }
                None => files.push((path, name, decorate.clone())),
            }
        } else {
            if settings.record_git_tree {
                if let Ok(Some((_, old_data))) = is_git_changed(&path) {
                    let tmpfile = write_to_temp_in(&old_data, parent, &mut tmp_files)?;
                    files.push((tmpfile, format!("{name} original"), decorate_orig.clone()));
                }
            }

            files.push((path, name, decorate.clone()));
        }
    }

    /*dbg!(files
    .iter()
    .map(|(tmp, name, ..)| (tmp, name))
    .collect::<Vec<_>>());*/

    std::thread::spawn(move || {
        let mut last_progress = 0.0;
        let result = debugrc
            .run_tases_fastforward(
                &files,
                settings.fastforward_speed,
                settings.run_as_merged,
                |status| {
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
                            status.current_file + 1,
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
                },
            )
            .map(|_| {
                // if let Err(e) = debugrc.get("cct/segmentRecording") {
                // eprintln!("Failed to segment recording: {e}");
                // }
            });

        for file in tmp_files {
            let _ = std::fs::remove_file(&file);
        }

        if record_ghost {
            let save_dir = celeste.save_dir().join("GhostsForTas");
            if save_dir.is_dir() {
                if let Ok(dir) = save_dir.read_dir() {
                    for item in dir {
                        if let Ok(item) = item {
                            let _ = std::fs::remove_file(item.path());
                        };
                    }
                }
            }
        }

        if settings.enable_tas_recorder {
            let settings = celeste.mod_settings("TASRecorder");
            let output_dir = settings
                .as_ref()
                .ok()
                .and_then(|settings| settings["OutputDirectory"].as_str())
                .map(Path::new);

            if let Some(out_dir) = output_dir {
                let path = celeste.path.join(out_dir);
                let _ = opener::open(&path);
            }
        }

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

pub fn check_required_mods(debugrc: &DebugRC) -> (bool, Option<String>, bool) {
    match debugrc.list_mods() {
        Ok(mods) => {
            let required_mods = ["CelesteTAS", "ConsistencyTracker"];
            let mut msg = String::new();
            for mod_name in required_mods {
                if !mods.iter().any(|m| m == mod_name) {
                    let _ = writeln!(&mut msg, "Mod `{}` is not installed. ", mod_name);
                }
            }
            let tas_recorder_installed = mods.iter().any(|m| m == "TASRecorder");
            (
                true,
                (!msg.is_empty()).then_some(msg),
                tas_recorder_installed,
            )
        }
        Err(_) => (false, None, true),
    }
}

fn write_to_temp_in(
    data: &str,
    tmp_dir: &Path,
    tmp_files: &mut Vec<PathBuf>,
) -> Result<PathBuf, anyhow::Error> {
    let name: String = "tmp_"
        .chars()
        .chain(std::iter::repeat_with(fastrand::alphabetic).take(12))
        .chain(".tas".chars())
        .collect::<String>();
    let file = tmp_dir.join(&name);

    std::fs::write(&file, data)?;
    tmp_files.push(file.clone());

    Ok(file)
}

fn with_old_new<T>(
    path: &Path,
    f: impl Fn(gix::Commit<'_>, &str, String) -> T,
) -> Result<Option<T>> {
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

    let data_new = std::fs::read_to_string(path)?;
    let data_old = std::str::from_utf8(&object.data)?;

    Ok(Some(f(head, data_old, data_new)))
}

/// returns (CommitPrefix, OldData)
fn is_git_changed(path: &Path) -> Result<Option<(String, String)>> {
    with_old_new(path, |commit, old, new| {
        let changed = old != new.replace("\r\n", "\n");
        let commit_id = commit
            .short_id()
            .map_or_else(|_| format!("{commit:?}"), |prefix| prefix.to_string());

        changed.then_some((commit_id, old.to_owned()))
    })
    .map(Option::flatten)
}

fn physics_log_in_diff(old: &str, new: &str, decorate: (String, String)) -> String {
    let mut first_line_changed = None;
    let mut first_line_changed_rev = None;
    let new_line_count = new.lines().count();

    let care_about_line = |line: &str| {
        let line = line.trim_start();
        !line.starts_with('#')
            && !line.starts_with("FileTime")
            && !line.starts_with("ChapterTime")
            && !line.starts_with("RecordCount")
    };

    for (i, (old, new)) in old.lines().zip(new.lines()).enumerate() {
        if first_line_changed.is_none() && old != new {
            if !care_about_line(old) && !care_about_line(new) {
                continue;
            }
            first_line_changed = Some(i);
        }
    }

    for (i, (old, new)) in old.lines().rev().zip(new.lines().rev()).enumerate() {
        if first_line_changed_rev.is_none() && old != new {
            if !care_about_line(old) && !care_about_line(new) {
                continue;
            }
            first_line_changed_rev = Some(i);
        }
    }

    let (Some(first_line_changed), Some(first_line_changed_rev)) =
        (first_line_changed, first_line_changed_rev)
    else {
        return new.into();
    };

    let (enable, disable) = decorate;

    let mut out = String::with_capacity(new.len() + disable.len() * 3);
    out.push_str(&disable);
    out.push('\n');

    for (i, line) in new.lines().enumerate() {
        if i == first_line_changed {
            out.push_str(&enable);
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
        if i == new_line_count - 1 - first_line_changed_rev {
            out.push_str(&disable);
            out.push('\n');
        }
    }

    out
}

#[test]
fn hi() {
    let enable = "Set,ConsistencyTracker.LogPhysicsEnabled,true";
    let disable = "Set,ConsistencyTracker.LogPhysicsEnabled,false";

    let result = physics_log_in_diff(
        "# Start
190
1,J
# lvl_1
2,J
# lvl_2
3,J
15,U,R,X
10,L
# lvl_end
4,J
ChapterTime:
",
        "# Start
190
1,J
# lvl_1
2,J
# lvl_2
10,U,R,X
2,R,K,G
10,L
# lvl_end
4,J
ChapterTime:
",
        (enable.into(), disable.into()),
    );
    println!("{}", result);
    assert_eq!(
        result,
        "Set,ConsistencyTracker.LogPhysicsEnabled,false
# Start
190
1,J
# lvl_1
2,J
# lvl_2
Set,ConsistencyTracker.LogPhysicsEnabled,true
10,U,R,X
2,R,K,G
Set,ConsistencyTracker.LogPhysicsEnabled,false
10,L
# lvl_end
4,J
ChapterTime:
"
    )
}
