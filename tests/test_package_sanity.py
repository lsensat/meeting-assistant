from pathlib import Path
import ast


ROOT = Path(__file__).resolve().parents[1]


def test_app_py_has_valid_python_syntax():
    source = (ROOT / "app.py").read_text(encoding="utf-8")
    ast.parse(source)


def test_runtime_uses_testable_audio_policy():
    source = (ROOT / "app.py").read_text(encoding="utf-8")
    assert "from audio_policy import (" in source
    assert "ordered_microphone_candidates(" in source
    assert "ordered_system_candidates(" in source


def test_audio_policy_has_no_gui_or_hardware_imports():
    source = (ROOT / "audio_policy.py").read_text(encoding="utf-8")
    tree = ast.parse(source)

    imported = set()

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            imported.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            imported.add(node.module)

    forbidden = {
        "tkinter",
        "customtkinter",
        "sounddevice",
        "pyaudiowpatch",
    }

    assert not (imported & forbidden)



def test_runtime_contains_hotplug_recovery_loops():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "choose_microphone_failover(" in source
    assert "choose_system_failover(" in source
    assert "HOTPLUG_CHECK_SECONDS" in source
    assert '"system_fallback"' in source


def test_recordings_use_stable_target_sample_rate():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "samplerate=TARGET_SAMPLE_RATE" in source
    assert "resample_mono(" in source



def test_transcription_ui_reports_percentage():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert '"transcribing_progress"' in source
    assert "transcription_percent(" in source
    assert "total_transcription_duration" in source



def test_finished_stage_uses_svg_check_icon():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "STAGE_DONE_ICON_B64" in source
    assert "image=stage_done_icon" in source


def test_failed_stage_uses_svg_cross_icon():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "STAGE_ERROR_ICON_B64" in source
    assert "image=stage_error_icon" in source


def test_config_is_loaded_before_ui_translation_is_used():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    config_pos = source.find("config = load_config()")
    ui_pos = source.find("root = ctk.CTk()")

    assert config_pos != -1
    assert ui_pos != -1
    assert config_pos < ui_pos


def test_current_language_has_safe_default_before_config_exists():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert 'globals().get("config", DEFAULT_CONFIG)' in source






def test_dropdown_uses_custom_svg_chevron():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "DROPDOWN_ARROW_ICON_B64" in source
    assert "image=dropdown_arrow_icon" in source
    assert "class StyledDropdown" in source



def test_bottom_toolbar_uses_space_between_layout():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("results_frame = ctk.CTkFrame(")
    end = source.index("# SETTINGS WINDOW", start)
    block = source[start:end]

    assert 'uniform="bottom_edges"' in block
    assert 'sticky="w"' in block
    assert 'sticky="e"' in block
    assert "center_results" in block






def test_hover_helper_has_no_early_color_default():
    source = (ROOT / "app.py").read_text(encoding="utf-8")
    tree = ast.parse(source)

    target = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef)
        and node.name == "bind_hover_content"
    )

    default_names = {
        node.id
        for default in (
            list(target.args.defaults)
            + list(target.args.kw_defaults)
        )
        if default is not None
        for node in ast.walk(default)
        if isinstance(node, ast.Name)
    }

    assert "COLOR_CREAM" not in default_names



def test_all_main_interactive_controls_get_pointer_cursor():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "def bind_pointer_cursor(" in source
    assert 'cursor="hand2"' in source

    for name in (
        "mute_button",
        "transcript_button",
        "summary_button",
        "folder_button",
        "settings_button",
        "refresh_ollama_button",
        "refresh_audio_button",
        "browse_button",
        "keep_audio_switch",
        "save_settings_button",
    ):
        assert name in source



def test_bootstrap_uses_app_py_entrypoint():
    source = (ROOT / "bootstrap.py").read_text(encoding="utf-8")

    assert 'app_py = APP_FOLDER / "app.py"' in source
    assert "meeting_py" not in source


def test_package_uses_new_app_filename():
    assert (ROOT / "app.py").exists()
    assert not (ROOT / "meeting.py").exists()




def test_dropdown_arrow_hover_matches_border_color():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("self.arrow_button = ctk.CTkButton(")
    end = source.index("self.arrow_button.place", start)
    block = source[start:end]

    assert "hover_color=COLOR_TAN" in block
    assert "image=dropdown_arrow_icon" in block







def test_shared_themed_button_class_exists():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "class ThemedButton(ctk.CTkButton):" in source
    assert "BUTTON_VARIANTS =" in source


def test_update_devices_and_change_folder_use_same_secondary_variant():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    update_start = source.index("refresh_audio_button = ThemedButton(")
    update_end = source.index("# FILES AND PRIVACY", update_start)
    update = source[update_start:update_end]

    browse_start = source.index("browse_button = ThemedButton(")
    browse_end = source.index("settings_keep_audio_var =", browse_start)
    browse = source[browse_start:browse_end]

    assert 'variant="secondary"' in update
    assert 'variant="secondary"' in browse


def test_secondary_variant_behavior_is_defined_once():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("BUTTON_VARIANTS =")
    end = source.index("class ThemedButton", start)
    block = source[start:end]

    assert '"fg": COLOR_TAN' in block
    assert '"hover": COLOR_SIENNA' in block
    assert '"text": COLOR_DARK_BROWN' in block
    assert '"hover_text": COLOR_CREAM' in block


def test_microphone_uses_its_shared_variant():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("mute_button = ThemedButton(")
    end = source.index("center_results =", start)
    block = source[start:end]

    assert 'variant="microphone"' in block
    assert "normal_image=mic_icon" in block
    assert "hover_image=mic_icon_hover" in block


def test_all_fields_share_same_bordered_container():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "class BorderedContainer(tk.Frame):" in source
    assert "self.frame = BorderedContainer(" in source
    assert "settings_output_border = BorderedContainer(" in source
    assert "settings_custom_prompt_border = BorderedContainer(" in source


def test_bordered_container_draws_one_pixel_on_all_sides():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("class BorderedContainer")
    end = source.index("BUTTON_VARIANTS =", start)
    block = source[start:end]

    assert "border_size=1" in block
    assert "bg=border_color" in block
    assert "width=self.field_width - 2 * self.border_size" in block
    assert "height=self.field_height - 2 * self.border_size" in block
    assert "x=self.border_size" in block
    assert "y=self.border_size" in block



def test_result_buttons_share_one_custom_class():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "class ResultButton(ThemedButton):" in source
    assert "transcript_button = ResultButton(" in source
    assert "summary_button = ResultButton(" in source
    assert "folder_button = ResultButton(" in source


def test_unavailable_results_match_disabled_stop_palette():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert 'COLOR_DISABLED_CONTROL = "#C9BBA8"' in source
    assert "COLOR_DISABLED_ICON = COLOR_CREAM" in source
    assert "COLOR_DISABLED_TEXT = COLOR_CREAM" in source

    start = source.index("class ResultButton")
    end = source.index("def style_option_menu", start)
    block = source[start:end]

    assert "fg_color=COLOR_DISABLED_CONTROL" in block
    assert "hover_color=COLOR_DISABLED_CONTROL" in block
    assert "text_color=COLOR_DISABLED_TEXT" in block


def test_available_results_share_exact_same_active_variant():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index('"result": {')
    end = source.index("}", start)
    variant = source[start:end]

    assert '"fg": COLOR_TAN' in variant
    assert '"hover": COLOR_SIENNA' in variant
    assert '"text": COLOR_DARK_BROWN' in variant
    assert '"hover_text": COLOR_CREAM' in variant

    result_start = source.index("class ResultButton")
    result_end = source.index("def style_option_menu", result_start)
    result_class = source[result_start:result_end]

    assert 'variant="result"' in result_class


def test_result_availability_is_stage_driven():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("def sync_result_button_with_stage")
    end = source.index("def apply_stage", start)
    block = source[start:end]

    assert '"audio": globals().get("folder_button")' in block
    assert '"whisper": globals().get("transcript_button")' in block
    assert '"summary": globals().get("summary_button")' in block
    assert 'state == "done"' in block


def test_result_buttons_reset_when_new_recording_starts():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("def start_recording")
    end = source.index("def stop_recording", start)
    block = source[start:end]

    assert "transcript_button.set_available(False)" in block
    assert "summary_button.set_available(False)" in block
    assert "folder_button.set_available(False)" in block



def test_disabled_summary_text_has_readable_contrast():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "COLOR_DISABLED_TEXT = COLOR_CREAM" in source

    start = source.index("class ResultButton")
    end = source.index("def style_option_menu", start)
    block = source[start:end]

    assert "text_color=COLOR_DISABLED_TEXT" in block



def test_installed_whisper_uses_shared_stage_check_icon():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert 'STAGE_DONE_ICON_B64' in source
    assert "return stage_done_icon" in source
    assert "icon_resolver=whisper_model_status_icon" in source


def test_installed_whisper_does_not_use_unicode_check():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert '"whisper_installed": "{model}  ✓ installed"' not in source
    assert '"whisper_installed": "{model}  ✓ instalado"' not in source


def test_styled_dropdown_supports_standard_option_icons():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    start = source.index("class StyledDropdown")
    end = source.index("def style_primary_button", start)
    block = source[start:end]

    assert "icon_resolver=None" in block
    assert "def _icon_for(" in block
    assert "image=self._icon_for(value)" in block
    assert 'compound="left"' in block



def test_result_button_disabled_text_uses_shared_cream_color():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "COLOR_DISABLED_TEXT = COLOR_CREAM" in source

    start = source.index("class ResultButton")
    end = source.index("def style_option_menu", start)
    block = source[start:end]

    assert "text_color=COLOR_DISABLED_TEXT" in block
    assert "text_color_disabled=COLOR_DISABLED_TEXT" in block


def test_all_three_result_buttons_use_result_button_class():
    source = (ROOT / "app.py").read_text(encoding="utf-8")

    assert "transcript_button = ResultButton(" in source
    assert "summary_button = ResultButton(" in source
    assert "folder_button = ResultButton(" in source
