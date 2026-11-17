#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(envmon_gui::APP_TITLE)
            .with_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        envmon_gui::APP_TITLE,
        options,
        Box::new(|_cc| Ok(Box::new(envmon_gui::MonitorApp::default()))),
    )
}
