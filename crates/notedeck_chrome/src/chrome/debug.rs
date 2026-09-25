//! Debug overlays shown from the sidebar under `--debug`: the repaint-cause
//! table and, with the `memory` feature, the allocator stats window.

use std::collections::HashMap;

#[cfg(feature = "memory")]
pub(super) fn memory_debug_ui(ui: &mut egui::Ui) {
    let Some(stats) = &re_memory::accounting_allocator::tracking_stats() else {
        ui.label("re_memory::accounting_allocator::set_tracking_callstacks(true); not set!!");
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(format!(
            "track_size_threshold {}",
            stats.track_size_threshold
        ));
        ui.label(format!(
            "untracked {} {}",
            stats.untracked.count,
            format_bytes(stats.untracked.size as f64)
        ));
        ui.label(format!(
            "stochastically_tracked {} {}",
            stats.stochastically_tracked.count,
            format_bytes(stats.stochastically_tracked.size as f64),
        ));
        ui.label(format!(
            "fully_tracked {} {}",
            stats.fully_tracked.count,
            format_bytes(stats.fully_tracked.size as f64)
        ));
        ui.label(format!(
            "overhead {} {}",
            stats.overhead.count,
            format_bytes(stats.overhead.size as f64)
        ));

        ui.separator();

        for (i, callstack) in stats.top_callstacks.iter().enumerate() {
            let full_bt = format!("{}", callstack.readable_backtrace);
            let mut lines = full_bt.lines().skip(5);
            let bt_header = lines.nth(0).map_or("??", |v| v);
            let header = format!(
                "#{} {bt_header} {}x {}",
                i + 1,
                callstack.extant.count,
                format_bytes(callstack.extant.size as f64)
            );

            egui::CollapsingHeader::new(header)
                .id_salt(("mem_cs", i))
                .show(ui, |ui| {
                    ui.label(lines.collect::<Vec<_>>().join("\n"));
                });
        }
    });
}

/// Pretty format a number of bytes by using SI notation (base2), e.g.
///
/// ```
/// # use re_format::format_bytes;
/// assert_eq!(format_bytes(123.0), "123 B");
/// assert_eq!(format_bytes(12_345.0), "12.1 KiB");
/// assert_eq!(format_bytes(1_234_567.0), "1.2 MiB");
/// assert_eq!(format_bytes(123_456_789.0), "118 MiB");
/// ```
#[cfg(feature = "memory")]
pub fn format_bytes(number_of_bytes: f64) -> String {
    /// The minus character: <https://www.compart.com/en/unicode/U+2212>
    /// Looks slightly different from the normal hyphen `-`.
    const MINUS: char = '−';

    if number_of_bytes < 0.0 {
        format!("{MINUS}{}", format_bytes(-number_of_bytes))
    } else if number_of_bytes == 0.0 {
        "0 B".to_owned()
    } else if number_of_bytes < 1.0 {
        format!("{number_of_bytes} B")
    } else if number_of_bytes < 20.0 {
        let is_integer = number_of_bytes.round() == number_of_bytes;
        if is_integer {
            format!("{number_of_bytes:.0} B")
        } else {
            format!("{number_of_bytes:.1} B")
        }
    } else if number_of_bytes < 10.0_f64.exp2() {
        format!("{number_of_bytes:.0} B")
    } else if number_of_bytes < 20.0_f64.exp2() {
        let decimals = (10.0 * number_of_bytes < 20.0_f64.exp2()) as usize;
        format!("{:.*} KiB", decimals, number_of_bytes / 10.0_f64.exp2())
    } else if number_of_bytes < 30.0_f64.exp2() {
        let decimals = (10.0 * number_of_bytes < 30.0_f64.exp2()) as usize;
        format!("{:.*} MiB", decimals, number_of_bytes / 20.0_f64.exp2())
    } else {
        let decimals = (10.0 * number_of_bytes < 40.0_f64.exp2()) as usize;
        format!("{:.*} GiB", decimals, number_of_bytes / 30.0_f64.exp2())
    }
}

pub(super) fn repaint_causes_window(ui: &mut egui::Ui, causes: &HashMap<egui::RepaintCause, u64>) {
    egui::Window::new("Repaint Causes").show(ui.ctx(), |ui| {
        use egui_extras::{Column, TableBuilder};
        TableBuilder::new(ui)
            .column(Column::auto().at_least(600.0).resizable(true))
            .column(Column::auto().at_least(50.0).resizable(true))
            .column(Column::auto().at_least(50.0).resizable(true))
            .column(Column::remainder())
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.heading("file");
                });
                header.col(|ui| {
                    ui.heading("line");
                });
                header.col(|ui| {
                    ui.heading("count");
                });
                header.col(|ui| {
                    ui.heading("reason");
                });
            })
            .body(|mut body| {
                for (cause, hits) in causes.iter() {
                    body.row(30.0, |mut row| {
                        row.col(|ui| {
                            ui.label(cause.file.to_string());
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", cause.line));
                        });
                        row.col(|ui| {
                            ui.label(format!("{hits}"));
                        });
                        row.col(|ui| {
                            ui.label(format!("{}", cause.reason));
                        });
                    });
                }
            });
    });
}
