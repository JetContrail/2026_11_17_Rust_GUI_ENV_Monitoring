//! UI state and rendering for the environment monitor.
//!
//! The UI is drawn by [`MonitorApp::show`], which only needs an [`egui::Ui`].
//! Tests drive it headlessly with `egui_kittest`, so no window or GPU is needed.

use envmon_core::{Level, Reading, Thresholds};

pub const APP_TITLE: &str = "Environment Monitor";
pub const SIMULATE_BUTTON: &str = "Simulate reading";
pub const CLEAR_BUTTON: &str = "Clear history";

const MAX_HISTORY: usize = 100;

const DEMO_READINGS: [Reading; 3] = [
    Reading {
        temperature_c: 22.5,
        humidity_pct: 45.0,
    },
    Reading {
        temperature_c: 32.0,
        humidity_pct: 60.0,
    },
    Reading {
        temperature_c: 41.0,
        humidity_pct: 88.0,
    },
];

/// UI state of the monitor window.
#[derive(Debug, Default)]
pub struct MonitorApp {
    thresholds: Thresholds,
    history: Vec<Reading>,
    demo_index: usize,
}

impl MonitorApp {
    pub fn latest(&self) -> Option<&Reading> {
        self.history.last()
    }

    pub fn history(&self) -> &[Reading] {
        &self.history
    }

    pub fn level(&self) -> Option<Level> {
        self.latest().map(|r| self.thresholds.evaluate(r))
    }

    pub fn push_reading(&mut self, reading: Reading) {
        if self.history.len() == MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(reading);
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    fn simulate(&mut self) {
        let reading = DEMO_READINGS[self.demo_index % DEMO_READINGS.len()];
        self.demo_index = self.demo_index.wrapping_add(1);
        self.push_reading(reading);
    }

    pub fn status_text(&self) -> String {
        match (self.latest(), self.level()) {
            (Some(r), Some(level)) => format!(
                "{level}: {:.1} °C / {:.1} %RH",
                r.temperature_c, r.humidity_pct
            ),
            _ => "No data".to_owned(),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.heading(APP_TITLE);
        ui.label(self.status_text());
        ui.horizontal(|ui| {
            if ui.button(SIMULATE_BUTTON).clicked() {
                self.simulate();
            }
            if ui.button(CLEAR_BUTTON).clicked() {
                self.clear();
            }
        });
        ui.label(format!("Samples: {}", self.history.len()));
    }
}

impl eframe::App for MonitorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| self.show(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded() {
        let mut app = MonitorApp::default();
        for _ in 0..MAX_HISTORY + 5 {
            app.simulate();
        }
        assert_eq!(app.history().len(), MAX_HISTORY);
    }

    #[test]
    fn status_reflects_latest_reading() {
        let mut app = MonitorApp::default();
        assert_eq!(app.status_text(), "No data");
        app.push_reading(DEMO_READINGS[2]);
        assert_eq!(app.level(), Some(Level::Alarm));
        assert!(app.status_text().starts_with("Alarm"));
    }
}
