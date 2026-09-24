fn main() {
    tauri_plugin::Builder::new(&[
        "monitor_state",
        "collector_request",
        "set_hit_regions",
        "rail_settings_get",
        "rail_settings_set",
        "close_settings",
        "open_view",
        "open_session_url",
        "open_integration_folder",
    ])
    .build();
}
