//! OpenCompose DAW — Tauri backend

mod ab_audition;
mod agent_api;
mod agent_tools;
mod ai;
mod ai_service;
mod audio;
mod audio_slot;
mod audition;
mod audition_figure;
mod autosave;
mod beat_grid;
mod build_progress;
mod clap_runtime;
mod commands;
mod content_library;
mod device_catalog;
mod device_presets;
mod diagnostics;
mod editor_helper;
mod first_block;
#[cfg(test)]
mod first_block_tests;
mod gui_settings;
mod input;
mod live_synth;
mod meters;
mod midi_bridge;
mod midi_commands;
mod midi_live;
mod midi_record;
mod native_jobs;
mod net_proxy;
mod open_events;
mod plugin_index;
mod plugin_inventory;
mod plugin_keepalive;
mod plugin_store;
mod preview;
mod preview_focus;
mod private_fs;
pub mod probe;
mod project_extent;
mod project_persist;
mod project_resources;
mod quit_intent;
mod record;
mod render_grid;
mod render_thread;
mod samples;
mod soundfont_store;
mod startup;
mod state;
mod tool_registry;
mod window_guard;
mod workspaces;

pub use startup::parse_path as parse_startup_path;
pub use startup::{DiagOptions, parse_diag_options};

use tauri::Manager;

use state::AppState;

/// Test-only phase timing for the ignored playback-latency harness.
#[cfg(test)]
pub(crate) fn profile_phase(label: &str, start: std::time::Instant) {
    if std::env::var_os("OCP_PROFILE_PLAYBACK").is_some() {
        eprintln!(
            "[profile] {label}_ms={:.2}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    run_with_project(None);
}

pub fn run_with_project(path: Option<String>) {
    run_with_options(path, DiagOptions::default());
}

pub fn run_with_options(path: Option<String>, diag: DiagOptions) {
    // Tauri leaves through `std::process::exit` after its event loop returns,
    // which runs a hosted module's static destructors; a Qt module dies there.
    // The guard is registered again once a plugin is instantiated, because
    // `atexit` handlers run in reverse registration order.
    oc_host::guard_plugin_teardown();
    // Framework logging can be configured by the operator's inherited
    // QT_LOGGING_RULES. A public library entry point cannot assume it is safe
    // to mutate the process environment, even before loading a plugin.
    let mode = startup::RunMode::from_environment();
    let mut context = tauri::generate_context!();
    mode.configure(context.config_mut());
    let builder = tauri::Builder::default();
    let builder = if mode.is_desktop() {
        // Must be the first plugin: later launches forward their file arguments
        // to this process instead of creating another unguarded document window.
        builder.plugin(tauri_plugin_single_instance::init(
            |app, argv, cwd| match startup::parse_path(
                argv.get(1..).unwrap_or_default(),
                std::path::Path::new(&cwd),
            ) {
                Ok(Some(path)) => open_events::queue_project(app, std::path::Path::new(&path)),
                Ok(None) => open_events::focus_main_window(app),
                Err(error) => log::warn!("project open: {error}"),
            },
        ))
    } else {
        builder
    };
    let app = builder
        .manage(mode)
        .manage(open_events::OpenProjectRequests::default())
        .manage(startup::StartupProject(std::sync::Mutex::new(path)))
        .manage(AppState::new())
        .manage(ai_service::Requests::default())
        .manage(ai_service::SeparatorJobs::default())
        .manage(ai::oauth::Attempts::default())
        .setup(move |app| {
            clap_runtime::ClapRuntime::install(app.handle())
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            if diag.any() {
                // `oc_diag::init` already installed the file logger in main();
                // a second logger would fight it.
                oc_diag::diag_info!(
                    "gui: start version={} debug={} perf={} features(debug-log={}, perf-log={}) os={} arch={} cpus={}",
                    env!("CARGO_PKG_VERSION"),
                    diag.debug,
                    diag.perf,
                    cfg!(feature = "debug-log"),
                    cfg!(feature = "perf-log"),
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(0)
                );
                oc_diag::spawn_reporter(std::time::Duration::from_secs(5));
            } else {
                // A development build still opens real projects, and every
                // per-load diagnostic at debug buries the warnings. `--debug`
                // turns them on; the default is the level a report needs.
                let log_level = if cfg!(debug_assertions) {
                    log::LevelFilter::Info
                } else {
                    log::LevelFilter::Warn
                };
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log_level)
                        // `debug` is about this application. A decoder or a
                        // windowing crate printing every internal step buries
                        // the lines a report needs, so another crate is capped
                        // at warnings unless it is asked for by name.
                        .filter(|metadata| {
                            metadata.level() <= log::Level::Warn
                                || metadata.target().starts_with("oc_")
                                || metadata.target().starts_with("opencompose")
                        })
                        .build(),
                )?;
            }
            // A menu bar is what gives the window its key equivalents on
            // macOS: without an Edit menu, Cmd+C never reaches the webview, so
            // selected text cannot be copied anywhere in the interface.
            #[cfg(target_os = "macos")]
            if let Ok(menu) = tauri::menu::Menu::default(app.handle())
                && let Err(error) = app.set_menu(menu)
            {
                log::warn!("menu: {error}");
            }
            app.handle().plugin(tauri_plugin_dialog::init())?;
            if let Ok(dir) = app.path().app_cache_dir() {
                let cap = oc_engine::media_cache::DEFAULT_DISK_BYTES;
                if let Err(error) = oc_engine::media_cache::init_shared(dir.join("media"), cap) {
                    log::warn!("media cache: {error}");
                }
            }
            // Reads the last-good cache from disk. Nothing fetches here: the
            // plugin browser asks for a refresh when it opens.
            let index = plugin_index::IndexClient::new(plugin_index::index_dir())
                .map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
            app.manage(index);
            app.manage(midi_commands::MidiJobs::default());
            // A deb or AppImage keeps resources away from the executable, so
            // the startup probe cannot see the bundled GM bank. Resolve it
            // through Tauri before any project loads a SoundFont preset.
            if let Ok(path) = app.path().resolve(
                "soundfonts/FluidR3Mono_GM.sf3",
                tauri::path::BaseDirectory::Resource,
            )
                && path.is_file()
            {
                let state = app.state::<AppState>();
                let mut inner = state.lock_inner();
                if inner.audio.soundfont_path.is_none() {
                    inner.audio.soundfont_path = Some(path.clone());
                }
                log::info!("soundfont: bundled GM bank at {}", path.display());
            }
            // A SoundFont preparation reports its stages into the build chip.
            build_progress::install_engine_reports();
            // Serve the stored plugin inventory before the first panel asks;
            // a moved key refreshes it off the UI thread.
            plugin_inventory::start(app.handle().clone());
            agent_api::spawn(app.handle().clone());
            autosave::spawn(app.handle().clone());
            // Desktop windows stay hidden until setup; headless hosts must not
            // claim desktop activation or execute a background frontend.
            if !mode.is_desktop() {
                log::info!("headless host: webviews={}", app.webview_windows().len());
                #[cfg(target_os = "macos")]
                {
                    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            } else if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ab_audition::get_ab_audition_state,
            startup::take_startup_project,
            open_events::peek_project_open_request,
            open_events::acknowledge_project_open_request,
            ai_service::ai_key_status,
            ai_service::ai_read_key,
            ai_service::ai_warm_key,
            ai_service::ai_set_key,
            ai_service::ai_remove_key,
            ai_service::ai_catalog,
            ai_service::ai_tools,
            ai_service::ai_toolsets,
            ai_service::ai_skills,
            ai_service::ai_attach_media,
            ai_service::ai_call_tool,
            ai_service::ai_oauth_status,
            ai_service::ai_oauth_begin,
            ai_service::ai_oauth_poll,
            ai_service::ai_oauth_cancel,
            ai_service::ai_oauth_sign_out,
            ai_service::ai_chat,
            ai_service::ai_name_session,
            ai_service::ai_cancel,
            ai_service::ai_cancel_separator,
            ai_service::ai_active_turn,
            commands::ai_model_limits,
            commands::ai_save_model_limits,
            commands::ai_reset_model_limits,
            commands::ai_add_provider,
            commands::ai_fetch_models,
            // AI conversations
            commands::sessions_list,
            commands::sessions_new,
            commands::sessions_load,
            commands::sessions_original,
            commands::sessions_snapshot,
            commands::sessions_rewind,
            commands::sessions_drop_history,
            commands::sessions_version,
            commands::sessions_rename,
            commands::sessions_set_permission,
            commands::sessions_delete,
            commands::sessions_compact,
            commands::sessions_checkpoint,
            // Workspaces
            commands::workspaces_list,
            commands::workspace_current,
            commands::workspace_open,
            commands::workspace_detached,
            commands::workspace_forget,
            // Preferences bundle
            commands::preferences_export,
            commands::preferences_import,
            // Build identity, render performance, storage
            commands::app_info,
            commands::performance_facts,
            commands::set_performance_settings,
            commands::storage_report,
            commands::reveal_path,
            commands::storage_scan,
            commands::session_audio_settings,
            commands::session_audio_save,
            commands::network_settings,
            commands::detect_local_proxy,
            commands::network_save,
            commands::network_test,
            commands::clear_sample_cache,
            commands::clear_clip_pcm_cache,
            commands::clear_audition_archive,
            commands::open_storage_folder,
            commands::open_url,
            // Sample marketplace
            commands::sample_market_providers,
            commands::sample_market_search,
            commands::sample_preview,
            commands::sample_market_add,
            commands::sample_market_save_key,
            commands::sample_market_key_status,
            commands::sample_market_remove_key,
            commands::freesound_oauth::freesound_oauth_begin,
            commands::freesound_oauth::freesound_oauth_status,
            commands::freesound_oauth::freesound_oauth_sign_out,
            // Project / DSL
            commands::new_project,
            commands::new_project_at,
            commands::open_file,
            commands::save_file,
            commands::get_project,
            autosave::list_recoveries,
            autosave::restore_recovery,
            autosave::discard_recovery,
            // MIDI import / export
            midi_commands::midi_import_preview,
            commands::import_aligned_stems,
            midi_commands::midi_import_apply,
            midi_commands::midi_import_progress,
            midi_commands::midi_import_cancel,
            midi_commands::midi_export_report,
            midi_commands::midi_export_write,
            // Live MIDI input
            midi_live::list_midi_ports,
            midi_live::get_midi_config,
            midi_live::set_midi_config,
            midi_live::get_midi_activity,
            commands::get_dsl_source,
            commands::parse_dsl,
            commands::update_dsl,
            // Track mutations
            commands::update_track_mix,
            commands::begin_mixer_touch,
            commands::update_mixer_touch,
            commands::track_latency_readout,
            commands::set_master_volume,
            commands::set_track_mute,
            commands::set_track_solo,
            commands::set_track_channel_overrides,
            commands::set_bus_mute,
            commands::set_bus_solo,
            commands::update_bus_mix,
            commands::add_track,
            commands::add_instrument_track,
            commands::duplicate_track,
            commands::remove_track,
            commands::rename_track,
            // Project properties
            commands::set_tempo,
            commands::set_tempo_map,
            commands::set_time_signature,
            commands::set_time_sig_map,
            commands::set_project_name,
            commands::set_export_settings,
            // Track preset
            commands::set_track_preset,
            commands::save_track_preset_as_new,
            commands::edit_performance_event,
            // Preset management
            commands::add_preset,
            commands::remove_preset,
            commands::set_preset_program,
            commands::set_preset_param,
            commands::set_preset_params,
            commands::set_preset_param_owned,
            commands::set_preset_params_owned,
            commands::synth_macro_learn,
            commands::synth_macro_learn_owned,
            // OpenSynth parameter automation
            commands::begin_synth_param_gesture,
            commands::update_synth_param_gesture,
            commands::end_synth_param_gesture,
            commands::begin_owned_synth_param_gesture,
            commands::begin_guarded_synth_param_gesture,
            commands::update_owned_synth_param_gesture,
            commands::end_owned_synth_param_gesture,
            commands::remove_synth_param_automation,
            commands::add_control_link_target,
            commands::update_control_link_target,
            commands::remove_control_link_target,
            // Live touch overrides for automated effect parameters
            commands::begin_guarded_effect_param_touch,
            commands::update_owned_effect_param_touch,
            commands::end_owned_effect_param_touch,
            commands::begin_effect_param_touch,
            commands::update_effect_param_touch,
            commands::end_effect_param_touch,
            commands::duplicate_preset,
            commands::rename_preset,
            commands::randomize_preset,
            commands::randomize_preset_owned,
            commands::list_library_patches,
            commands::list_patch_preferences,
            commands::set_patch_favorite,
            commands::save_patch_to_library,
            commands::remove_library_patch,
            commands::export_patch_bundle,
            commands::picture_timecode,
            commands::set_picture_clock,
            commands::import_patch_bundle,
            // Note editing
            commands::add_note,
            commands::remove_note,
            commands::add_phrase,
            commands::insert_phrase,
            commands::insert_inline_pattern,
            commands::extend_phrase_range,
            commands::move_note,
            commands::set_note_duration,
            commands::set_note_velocity,
            commands::clear_note_velocity,
            commands::set_note_dynamics,
            commands::set_note_articulation,
            commands::set_note_muted,
            commands::set_note_grace,
            // Pattern note editing
            commands::add_pattern_note,
            commands::remove_pattern_note,
            commands::move_pattern_note,
            commands::set_pattern_note_velocity,
            commands::clear_pattern_note_velocity,
            commands::set_pattern_hit_velocity,
            commands::clear_pattern_hit_velocity,
            commands::set_pattern_note_dynamics,
            commands::set_pattern_note_articulation,
            commands::set_pattern_note_muted,
            commands::set_pattern_note_grace,
            commands::set_pattern_note_duration,
            // Batch note tools
            commands::add_notes,
            commands::duplicate_notes,
            commands::move_notes,
            commands::remove_notes,
            commands::set_note_velocities,
            commands::quantise_notes,
            commands::humanise_notes,
            commands::legato_notes,
            commands::ramp_note_velocity,
            commands::scale_note_velocity,
            commands::compress_note_velocity,
            // Pattern management
            commands::add_pattern,
            commands::remove_pattern,
            commands::rename_pattern,
            // Pattern property editing
            commands::set_pattern_preset,
            commands::set_pattern_transpose,
            commands::set_pattern_swing,
            commands::set_project_swing_subdivision,
            commands::set_pattern_color,
            commands::set_pattern_bars,
            commands::trim_pattern_to_bars,
            // Pattern hit editing
            commands::add_pattern_hit,
            commands::remove_pattern_hit,
            commands::move_pattern_hit,
            // Clip management
            commands::add_clip,
            commands::remove_clip,
            commands::split_clip,
            commands::remove_blocks,
            commands::move_clip,
            commands::move_clip_group,
            commands::move_clip_to_track,
            commands::group_clips,
            commands::ungroup_clips,
            commands::resize_clip,
            commands::move_phrase,
            commands::move_phrase_to_track,
            commands::move_inline_pattern,
            commands::move_inline_pattern_to_track,
            commands::audio_clip_peaks,
            commands::audio_clip_transients,
            commands::move_audio_clip,
            commands::trim_audio_clip,
            commands::audio_clip_set_placement,
            commands::split_audio_clip,
            commands::remove_audio_clip,
            commands::comp_audio_takes,
            commands::set_audio_take_mute,
            commands::set_take_solo,
            commands::set_audio_clip_envelope,
            commands::set_audio_clip_offset,
            commands::set_audio_clip_playback,
            commands::set_pattern_clip_offset,
            commands::set_audio_clip_loop,
            commands::set_pattern_clip_loop,
            commands::bounce_track_to_audio,
            commands::consolidate_selection,
            // Arrangement sections
            commands::add_section,
            commands::remove_section,
            // Timeline markers
            commands::add_marker,
            commands::move_marker,
            commands::rename_marker,
            commands::remove_marker,
            // Send routing
            commands::add_send,
            commands::add_control_route,
            commands::remove_control_route,
            commands::set_send,
            commands::set_send_target,
            commands::set_track_output,
            commands::set_track_output_port,
            commands::remove_send,
            // Effects / buses / plugins
            commands::devices_catalog,
            commands::meter_snapshot,
            commands::set_monitor_mono,
            commands::reset_meter_clip,
            commands::set_effect_sidechain,
            commands::set_spectrum,
            commands::add_effect,
            commands::remove_effect,
            commands::move_effect,
            commands::set_effect_bypass,
            commands::set_effect_bypass_owned,
            commands::set_effect_param,
            commands::set_effect_param_owned,
            commands::set_effect_params,
            commands::set_effect_params_owned,
            // Device preset library
            device_presets::device_presets,
            device_presets::save_device_preset,
            device_presets::delete_device_preset,
            device_presets::apply_device_preset,
            commands::set_automation_smoothing,
            commands::set_automation_point_curve,
            commands::set_automation_mode,
            commands::set_automation_lfo,
            commands::add_automation_point,
            commands::remove_automation_point,
            commands::move_automation_point,
            commands::set_automation_lane_mute,
            commands::move_automation_clip,
            commands::resize_automation_clip,
            commands::duplicate_automation_clip,
            commands::split_automation_clip,
            commands::delete_automation_clip,
            commands::remove_automation_lane,
            commands::automation_value_at,
            commands::begin_effect_param_write,
            commands::begin_owned_effect_param_write,
            commands::begin_guarded_effect_param_write,
            commands::update_effect_param_write,
            commands::update_owned_effect_param_write,
            commands::end_effect_param_write,
            commands::end_owned_effect_param_write,
            commands::add_bus,
            commands::set_bus_routes,
            commands::remove_bus,
            commands::scan_plugins,
            commands::plugin_catalog,
            commands::plugin_search,
            commands::plugin_store_open,
            commands::plugin_store_install,
            commands::install_cancel,
            commands::plugin_manual_page,
            commands::plugin_installed_check,
            commands::plugin_index_status,
            commands::plugin_index_refresh,
            commands::soundfonts_list,
            commands::soundfont_install,
            commands::soundfonts_remove,
            commands::list_sampler_assets,
            content_library::list_content_libraries,
            content_library::register_content_library,
            content_library::remove_content_library,
            content_library::rescan_content_libraries,
            commands::project_compatibility_report,
            commands::load_plugin_for_preset,
            commands::ensure_plugin_instance,
            commands::plugin_parameter_panel,
            commands::preset_output_pairs,
            commands::preset_output_ports,
            commands::get_plugin_params,
            commands::get_plugin_programs,
            commands::plugin_program_capabilities,
            commands::select_plugin_program,
            commands::import_ffp_preset,
            commands::begin_plugin_param_gesture,
            commands::begin_owned_plugin_param_gesture,
            commands::begin_guarded_plugin_param_gesture,
            commands::update_plugin_param_gesture,
            commands::update_owned_plugin_param_gesture,
            commands::end_plugin_param_gesture,
            commands::end_owned_plugin_param_gesture,
            commands::save_plugin_state_file,
            commands::save_plugin_state_embedded,
            commands::apply_plugin_state,
            commands::migrate_vst2_presets_to_vst3,
            commands::open_plugin_gui,
            commands::close_plugin_gui,
            commands::plugin_has_gui,
            commands::plugin_editor_status,
            commands::poll_plugin_gui_closed,
            commands::poll_plugin_host_events,
            // Audio / transport
            commands::preview_preset,
            commands::audition_factory_patch,
            commands::compare_factory_patches,
            commands::switch_ab,
            commands::preview_pattern,
            commands::stop_audition,
            commands::pause_audition,
            commands::resume_audition,
            commands::preview_drum_pad,
            commands::open_synth_note_on,
            commands::open_synth_note_off,
            commands::warm_open_synth_keyboard,
            commands::synth_waveform_preview,
            commands::preview_track,
            commands::resume_track_preview,
            commands::preview_block,
            commands::render_offline,
            commands::audio_file_preview,
            commands::preview_audio_clip_source,
            commands::preview_saved_audition,
            commands::open_saved_audition_render,
            commands::render_stems,
            commands::render_audio,
            commands::cancel_export,
            commands::export_target_exists,
            commands::export_capabilities,
            commands::reveal_export,
            commands::transport_play,
            commands::transport_pause,
            commands::transport_stop,
            commands::transport_seek,
            commands::transport_mute,
            commands::transport_unmute,
            commands::get_transport_state,
            commands::project_build_progress,
            preview_focus::claim_web_audio,
            preview_focus::release_web_audio,
            commands::set_loop_region,
            commands::set_punch_region,
            // Undo / redo
            commands::undo,
            commands::redo,
            commands::get_undo_state,
            commands::get_undo_history,
            commands::restore_undo_level,
            commands::begin_undo_gesture,
            commands::end_undo_gesture,
            commands::begin_owned_undo_gesture,
            commands::begin_parameter_gesture,
            commands::end_owned_undo_gesture,
            // AI tool
            commands::ai_build_context,
            commands::ai_build_generate_context,
            commands::ai_validate_edit,
            commands::ai_apply_edit,
            commands::ai_batch_recover,
            // Edit preview: the proposal as a document, side by side in time
            commands::preview_begin,
            commands::preview_show,
            commands::preview_accept,
            commands::preview_cancel,
            commands::preview_state,
            commands::import_external_preset,
            commands::extract_subtree,
            commands::diff_dsl,
            // Per-project AI instructions
            commands::ai_project_instructions_get,
            commands::ai_project_instructions_set,
            // Audio device
            commands::list_audio_io,
            commands::get_audio_io,
            commands::set_audio_io,
            commands::arm_track,
            commands::start_recording,
            commands::stop_recording,
            midi_record::set_midi_record_settings,
            commands::set_click,
            commands::set_input_io,
            commands::set_input_monitor,
            commands::list_audio_devices,
            commands::get_audio_config,
            // AI config
            commands::load_ai_config,
            commands::save_ai_config,
            diagnostics::export_diagnostics,
            diagnostics::reveal_diagnostics,
            quit_intent::discard_unsaved,
            quit_intent::prepare_quit,
            quit_intent::quit_now,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                quit_intent::intercept_window_close(window, api);
            }
            if matches!(event, tauri::WindowEvent::Destroyed)
                && let Some(state) = window.try_state::<AppState>()
            {
                state.preview_focus.window_closed(window.label());
            }
        })
        .build(context)
        .expect("error while running tauri application");
    // The panel owns the save-or-discard decision: the window close handler asks
    // it, and a quit asks it from here. Neither event writes the document, so a
    // discarded project never reaches the file.
    #[cfg(target_os = "macos")]
    {
        quit_intent::install_quit_item(&app);
        app.on_menu_event(|handle, event| {
            if event.id() == quit_intent::QUIT_MENU_ID {
                quit_intent::request(handle);
            }
        });
    }
    app.run(move |handle, event| match event {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Opened { urls } if mode.is_desktop() => {
            for url in urls {
                match open_events::path_from_url(&url) {
                    Ok(path) => open_events::queue_project(handle, std::path::Path::new(&path)),
                    Err(error) => log::warn!("project open: {error}"),
                }
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => open_events::focus_main_window(handle),
        tauri::RunEvent::ExitRequested { api, .. } => quit_intent::intercept_exit(handle, &api),
        // The last-resort write, armed by `quit_now` alone.
        tauri::RunEvent::Exit => {
            // The endpoint file names a port that stops listening with this
            // process, so a later client must not find it.
            agent_api::shutdown();
            clap_runtime::ClapRuntime::shutdown(handle);
            project_persist::flush_on_exit(handle);
            // Nothing an editor helper opened may outlive the app: quitting is
            // the last chance to stop a child that owns its own plugin window.
            if let Some(state) = handle.try_state::<AppState>() {
                state.lock_inner().close_editor_helpers();
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod test_alloc;
