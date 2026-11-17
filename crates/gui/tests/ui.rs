//! Headless UI tests: render the real UI with egui_kittest and interact via AccessKit.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use envmon_core::Level;
use envmon_gui::{CLEAR_BUTTON, MonitorApp, SIMULATE_BUTTON};

fn harness() -> Harness<'static, MonitorApp> {
    Harness::new_ui_state(
        |ui, app: &mut MonitorApp| app.show(ui),
        MonitorApp::default(),
    )
}

#[test]
fn initial_screen_shows_no_data() {
    let harness = harness();
    harness.get_by_label("No data");
    harness.get_by_label("Samples: 0");
}

#[test]
fn simulate_button_adds_readings_and_updates_status() {
    let mut harness = harness();

    harness.get_by_label(SIMULATE_BUTTON).click();
    harness.run();
    assert_eq!(harness.state().level(), Some(Level::Normal));
    harness.get_by_label("Samples: 1");

    harness.get_by_label(SIMULATE_BUTTON).click();
    harness.run();
    assert_eq!(harness.state().level(), Some(Level::Warning));
    harness.get_by_label("Samples: 2");
}

#[test]
fn clear_button_resets_history() {
    let mut harness = harness();
    harness.get_by_label(SIMULATE_BUTTON).click();
    harness.run();
    harness.get_by_label(CLEAR_BUTTON).click();
    harness.run();
    assert!(harness.state().history().is_empty());
    harness.get_by_label("No data");
}
