# Results: rust-rewrite
**Updated:** 2026-11-10 07:08:03Z
**Review:** open: 9

| eval | result | evidence |
|------|--------|----------|
| E1 | pass | evidence/E1-build.txt: build rc=0, ELF PIE, libpython links 0, Cargo.lock tracked, clippy -D warnings rc=0, async/python deps 0 |
| E2 | pass | evidence/E2-startup.txt: ready in 1222/1072/1088 ms, no window at boot, methods .CycleProfile .CycleProfileBack .Open .OpenWindow, stdout 0 bytes, panics 0 |
| E3 | pass | evidence/E3-hostile-env.txt: corrupt config left untouched, runs with no tools, answers while tools hang, window within 2 s, panics 0 |
| E4 | pass | evidence/E4-flags.txt: no service -> rc=0 with 'ROG Control service not reachable'; open shown/hidden; forward set Quiet+power-saver, back set Balanced+balanced |
| E5 | pass | evidence/E5-launcher.txt: 8 launches rc=0, processes: 1, first instance alive and answering |
| E6 | pass | evidence/E6-tray.txt: late watcher found (:1.3/StatusNotifierItem), properties correct, Activate toggles, scroll logs asusctl profile set Quiet, exits on SecondaryActivate |
| E7 | pass | evidence/E7-main-window.txt: 494x913 both, images evidence/E7-main-window-rust.png and evidence/E7-main-window-python-ref-live.png;, imgsame 0.0010 0.0017 look-ok, stderr offenders 0 |
| E8 | pass | evidence/E8-companions.txt: each opens once (images evidence/E8-fans-and-power.png evidence/E8-keyboard-lighting.png evidence/E8-extra.png), look-ok, sizes 560x760/380x480/453x320, x=40; main close hides |
| E9 | pass | evidence/E9-hostile-dbus.txt: 7 bad calls rc=1 with D-Bus errors, 20 concurrent cycles -> 20 writes, alive after 50 Open calls, panics 0 |
| E10 | pass | evidence/E10-polling.txt: hidden 10 s tool calls=0 cpu ticks=1; visible 6 s temperature polls=3; hidden again 0 |
| E11 | pass | evidence/E11-automation-parity.txt: Python and Rust startup command sets identical (6 lines, writes: 5), config untouched |
| E12 | pass | evidence/E12-test-suite.txt: python tests 67, ported 67, names identical; cargo test 98 passed 0 failed; non-read tool calls: none; real config hash OK |
| E13 | pass | evidence/E13-mutation-check.txt: baseline 98 passed; every mutation (rows 1-11, plus extra samples) produced at least one FAILED test |
| E14 | pass | evidence/E14-install.txt: clean-home install rc=0, unit/desktop differ only in exec path, icons present, systemctl calls daemon-reload,enable,restart |
| E15 | pass | evidence/E15-checkout-gone.txt: installed binary and tray icon work with the checkout moved away |
| E16 | pass | evidence/E16-idempotent.txt: second install changed nothing, backups exact and never overwritten |
| E17 | pass | evidence/E17-failed-build-prereqs.txt: failed build and missing prerequisites change nothing and name what is missing |
| E18 | pass | evidence/E18-uninstall.txt: uninstall removes only installed files, leaves .pre-rust backups, second uninstall rc=0 |
| E19 | pass | evidence/E19-keybinds.txt: reverse-edit hash matches original, 108 lines, 4 binds as required, lua parses, configerrors [] |
| E20 | fail | evidence/E20-hardware-unchanged.txt: diff hw.before hw.after differs on one line: eDP-1 refreshRate 240.00301 -> 60.00000 (likely E19's `hyprctl reload` re-applying the compositor monitor rule; back to 240 after E21 restarted the service). All other hardware lines and the real config hash match. |
| E21 | pass | evidence/E21-real-deployment.txt: service active on ~/.local/bin/rog-control, python processes 0, rog-control processes 1, both keybinds and the launcher open/close the window, journal panics 0 |
| E22 | fail | evidence/E22-docs-python.txt: py outside specs: 4 (config.py main.py tests/__init__.py tests/test_e2e.py); git status not clean (RD.md, harness/lib.sh modified, evidence/ untracked); CLAUDE.md, history, target-ignore and branch checks OK |

## Round 1 notes
Tester: super-agents:tester at HEAD dd6f041. The tester's wording is copied above; E20 and E22 are FAIL as the tester reported them.

## FR-38 Python-to-Rust test mapping (67 rows)
| Python test | Rust test |
|---|---|
| TestSysfsHelpers.test_read_sysfs_missing_file_returns_default | tests/e2e/ported.rs::sysfs_helpers__read_sysfs_missing_file_returns_default |
| TestSysfsHelpers.test_read_sysfs_none_path_returns_default | tests/e2e/ported.rs::sysfs_helpers__read_sysfs_none_path_returns_default |
| TestSysfsHelpers.test_read_sysfs_int_non_numeric_returns_default | tests/e2e/ported.rs::sysfs_helpers__read_sysfs_int_non_numeric_returns_default |
| TestHwmonResolution.test_cpu_temp_path_is_coretemp_not_hardcoded | tests/e2e/ported.rs::hwmon_resolution__cpu_temp_path_is_coretemp_not_hardcoded |
| TestHwmonResolution.test_fan_paths_are_asus_driver | tests/e2e/ported.rs::hwmon_resolution__fan_paths_are_asus_driver |
| TestHwmonResolution.test_resolved_sensors_actually_readable | tests/e2e/ported.rs::hwmon_resolution__resolved_sensors_actually_readable |
| TestColourHelpers.test_hex_gdk_rgba_round_trip | tests/e2e/ported.rs::colour_helpers__hex_gdk_rgba_round_trip |
| TestColourHelpers.test_theme_colours_match_live_scheme | tests/e2e/ported.rs::colour_helpers__theme_colours_match_live_scheme |
| TestAuraModeMapping.test_index_maps_are_exact_inverses | tests/e2e/ported.rs::aura_mode_mapping__index_maps_are_exact_inverses |
| TestAuraModeMapping.test_every_effect_has_a_mode_index | tests/e2e/ported.rs::aura_mode_mapping__every_effect_has_a_mode_index |
| TestCurrentMonitorPicksInternalPanel.test_picks_edp_even_when_listed_second | tests/e2e/ported.rs::current_monitor__picks_edp_even_when_listed_second |
| TestCurrentMonitorPicksInternalPanel.test_picks_edp_even_when_listed_first | tests/e2e/ported.rs::current_monitor__picks_edp_even_when_listed_first |
| TestCurrentMonitorPicksInternalPanel.test_falls_back_to_first_monitor_when_no_internal_panel | tests/e2e/ported.rs::current_monitor__falls_back_to_first_monitor_when_no_internal_panel |
| TestApplyScreenPresetUsesEval.test_uses_hyprctl_eval_not_keyword | tests/e2e/ported.rs::screen_preset__uses_hyprctl_eval_not_keyword |
| TestApplyScreenPresetUsesEval.test_preserves_position_and_output_name | tests/e2e/ported.rs::screen_preset__preserves_position_and_output_name |
| TestGpuModeLogic.test_current_gpu_mode_ultimate_when_mux_zero | tests/e2e/ported.rs::gpu_mode__current_gpu_mode_ultimate_when_mux_zero |
| TestGpuModeLogic.test_current_gpu_mode_optimized_when_flag_set_and_mux_one | tests/e2e/ported.rs::gpu_mode__current_gpu_mode_optimized_when_flag_set_and_mux_one |
| TestGpuModeLogic.test_current_gpu_mode_eco_vs_standard_by_dgpu_disable | tests/e2e/ported.rs::gpu_mode__current_gpu_mode_eco_vs_standard_by_dgpu_disable |
| TestGpuModeLogic.test_gpu_mode_needs_reboot_only_for_mux_change | tests/e2e/ported.rs::gpu_mode__needs_reboot_only_for_mux_change |
| TestGpuModeLogic.test_apply_gpu_mode_never_touches_mux_when_already_correct | tests/e2e/ported.rs::gpu_mode__apply_never_touches_mux_when_already_correct |
| TestGpuModeLogic.test_apply_gpu_mode_never_touches_dgpu_disable_in_same_call_as_a_mux_change | tests/e2e/ported.rs::gpu_mode__apply_never_touches_dgpu_disable_in_same_call_as_a_mux_change |
| TestGpuModeLogic.test_refuses_to_disable_dgpu_when_external_display_attached | tests/e2e/ported.rs::gpu_mode__refuses_to_disable_dgpu_when_external_display_attached |
| TestGpuModeLogic.test_proceeds_normally_when_no_external_display_on_dgpu | tests/e2e/ported.rs::gpu_mode__proceeds_normally_when_no_external_display_on_dgpu |
| TestExternalDisplayOnDgpu.test_matches_real_hardware_state | tests/e2e/ported.rs::external_display__matches_real_hardware_state |
| TestExternalDisplayOnDgpu.test_internal_panel_alone_is_not_treated_as_external | tests/e2e/ported.rs::external_display__internal_panel_alone_is_not_treated_as_external |
| TestExternalDisplayOnDgpu.test_non_edp_connected_on_nvidia_card_is_external | tests/e2e/ported.rs::external_display__non_edp_connected_on_nvidia_card_is_external |
| TestExternalDisplayOnDgpu.test_ignores_non_nvidia_cards | tests/e2e/ported.rs::external_display__ignores_non_nvidia_cards |
| TestProfilePowerPlan.test_every_profile_has_a_power_plan | tests/e2e/ported.rs::profile_power_plan__every_profile_has_a_power_plan |
| TestProfilePowerPlan.test_apply_profile_pairs_power_profile | tests/e2e/ported.rs::profile_power_plan__apply_profile_pairs_power_profile |
| TestDebouncer.test_rapid_triggers_coalesce_to_one_call | tests/e2e/ported.rs::debouncer__rapid_triggers_coalesce_to_one_call |
| TestDebouncer.test_separated_triggers_each_fire | tests/e2e/ported.rs::debouncer__separated_triggers_each_fire |
| TestNvidiaSmiExitCodeLie.test_not_supported_text_treated_as_failure_despite_exit_zero | tests/e2e/ported.rs::nvidia_smi__not_supported_text_treated_as_failure_despite_exit_zero |
| TestNvidiaSmiExitCodeLie.test_real_success_with_exit_zero_and_no_warning | tests/e2e/ported.rs::nvidia_smi__real_success_with_exit_zero_and_no_warning |
| TestFanCurveGraphMonotonicSpeed.test_drag_cannot_produce_non_monotonic_speed | tests/e2e/ported.rs::fan_curve_graph__drag_cannot_produce_non_monotonic_speed |
| TestFanCurveGraphMonotonicSpeed.test_resulting_curve_is_accepted_by_the_real_monotonic_check | tests/e2e/ported.rs::fan_curve_graph__resulting_curve_is_accepted_by_the_real_monotonic_check |
| TestFactoryDefaultsOnlyResetsActiveProfile.test_refuses_when_edited_profile_is_not_active | tests/e2e/ported.rs::factory_defaults__refuses_when_edited_profile_is_not_active |
| TestFactoryDefaultsOnlyResetsActiveProfile.test_proceeds_when_edited_profile_is_active | tests/e2e/ported.rs::factory_defaults__proceeds_when_edited_profile_is_active |
| TestFanCurveParsing.test_parses_cpu_curve | tests/e2e/ported.rs::fan_curve_parsing__parses_cpu_curve |
| TestFanCurveParsing.test_parses_gpu_curve_independently | tests/e2e/ported.rs::fan_curve_parsing__parses_gpu_curve_independently |
| TestFanCurveParsing.test_missing_fan_falls_back_to_default | tests/e2e/ported.rs::fan_curve_parsing__missing_fan_falls_back_to_default |
| TestConfigPersistence.test_defaults_present_after_load_with_empty_file | tests/e2e/ported.rs::config_persistence__defaults_present_after_load_with_empty_file |
| TestConfigPersistence.test_merge_preserves_existing_values | tests/e2e/ported.rs::config_persistence__merge_preserves_existing_values |
| TestConfigPersistence.test_save_load_round_trip | tests/e2e/ported.rs::config_persistence__save_load_round_trip |
| TestWidgetConstruction.test_performance_panel | tests/e2e/ported.rs::widget_construction__performance_panel |
| TestWidgetConstruction.test_gpu_panel | tests/e2e/ported.rs::widget_construction__gpu_panel |
| TestWidgetConstruction.test_screen_panel | tests/e2e/ported.rs::widget_construction__screen_panel |
| TestWidgetConstruction.test_keyboard_panel | tests/e2e/ported.rs::widget_construction__keyboard_panel |
| TestWidgetConstruction.test_battery_panel | tests/e2e/ported.rs::widget_construction__battery_panel |
| TestWidgetConstruction.test_startup_panel | tests/e2e/ported.rs::widget_construction__startup_panel |
| TestWidgetConstruction.test_footer_panel | tests/e2e/ported.rs::widget_construction__footer_panel |
| TestWidgetConstruction.test_fans_power_window | tests/e2e/ported.rs::widget_construction__fans_power_window |
| TestWidgetConstruction.test_lighting_window | tests/e2e/ported.rs::widget_construction__lighting_window |
| TestWidgetConstruction.test_extra_window | tests/e2e/ported.rs::widget_construction__extra_window |
| TestWidgetConstruction.test_full_rog_popup | tests/e2e/ported.rs::widget_construction__full_rog_popup |
| TestGpuTempNoFlicker.test_refresh_never_shows_placeholder_after_first_real_reading | tests/e2e/ported.rs::gpu_temp__refresh_never_shows_placeholder_after_first_real_reading |
| TestModeSelectorVisibility.test_hidden_on_power_shown_on_fan_curve | tests/e2e/ported.rs::mode_selector__hidden_on_power_shown_on_fan_curve |
| TestSliderDebounce.test_pl1_rapid_drag_sends_one_command | tests/e2e/ported.rs::slider_debounce__pl1_rapid_drag_sends_one_command |
| TestRgbDebounceAndPreservation.test_rapid_multi_field_change_sends_one_command_with_final_values | tests/e2e/ported.rs::rgb__rapid_multi_field_change_sends_one_command_with_final_values |
| TestRgbDebounceAndPreservation.test_reset_to_theme_switches_to_static_when_effect_has_no_colour | tests/e2e/ported.rs::rgb__reset_to_theme_switches_to_static_when_effect_has_no_colour |
| TestPowerLimitsPersistAcrossReopen.test_pl1_value_survives_window_close_and_reopen | tests/e2e/ported.rs::power_limits_persist__pl1_value_survives_window_close_and_reopen |
| TestPowerLimitsPersistAcrossReopen.test_apply_limits_switch_persists | tests/e2e/ported.rs::power_limits_persist__apply_limits_switch_persists |
| TestBatteryScaleRange.test_range_matches_asusctl_supported_range | tests/e2e/ported.rs::battery_scale__range_matches_asusctl_supported_range |
| TestNoDynamicDockingLeftover.test_open_dock_left_function_removed | tests/e2e/ported.rs::no_dynamic_docking__open_dock_left_function_removed |
| TestHyprlandRulesSyntax.test_rules_file_parses_with_balanced_delimiters | tests/e2e/ported.rs::hyprland_rules__rules_file_parses_with_balanced_delimiters |
| TestHyprlandRulesSyntax.test_main_window_and_companion_rules_present | tests/e2e/ported.rs::hyprland_rules__main_window_and_companion_rules_present |
| TestAppStartup.test_starts_and_answers_within_budget | tests/e2e/startup.rs::app_startup__starts_and_answers_within_budget |
| TestAppStartup.test_launching_again_while_running_does_not_hang | tests/e2e/startup.rs::app_startup__launching_again_while_running_does_not_hang |
