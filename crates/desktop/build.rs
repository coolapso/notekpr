//! Tauri's build step. The app's commands are declared here so that only
//! what capabilities/main.json grants (the app's own window) may call them.

const COMMANDS: &[&str] = &[
    "desktop_info",
    "add_server",
    "switch_account",
    "remove_account",
    "account_seen",
    "use_this_computer",
    "local_start",
    "local_session",
    "sign_in",
    "cancel_sign_in",
    "set_close_to_tray",
    "set_autostart",
    "open_external",
    "choose_folder",
    "folder_start",
    "folder_off",
    "folder_retry",
    "reveal_folder",
    "session_changed",
    "signed_out",
    "forget_computer",
    "folder_bring",
    "cli_folders",
    "folder_take_from_cli",
    "cancel_take",
    "setup_done",
    "dismiss_update",
    "update_help",
    "next_event",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("Tauri's build step");
}
