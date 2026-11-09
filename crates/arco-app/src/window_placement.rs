use arco_core::profile::{DisplayPlacement, WindowPlacement};
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit::monitor::MonitorHandle;

fn display_key(monitor: &MonitorHandle) -> String {
    monitor.name().unwrap_or_else(|| {
        let position = monitor.position();
        format!("Anzeige@{},{}", position.x, position.y)
    })
}

pub fn capture(window: &slint::Window, placement: &mut WindowPlacement) {
    window.with_winit_window(|window| {
        let Some(monitor) = window.current_monitor() else {
            return;
        };
        if window.is_minimized() == Some(true) {
            return;
        }
        let display = display_key(&monitor);
        if window.is_maximized() {
            placement
                .displays
                .entry(display.clone())
                .or_insert(DisplayPlacement {
                    offset: [40, 40],
                    size: [1200, 760],
                    maximized: true,
                })
                .maximized = true;
        } else {
            let Ok(position) = window.outer_position() else {
                return;
            };
            let origin = monitor.position();
            let scale = monitor.scale_factor();
            let size = window.inner_size().to_logical::<f64>(scale);
            placement.displays.insert(
                display.clone(),
                DisplayPlacement {
                    offset: [
                        (f64::from(position.x.saturating_sub(origin.x)) / scale).round() as i32,
                        (f64::from(position.y.saturating_sub(origin.y)) / scale).round() as i32,
                    ],
                    size: [size.width.round() as u32, size.height.round() as u32],
                    maximized: false,
                },
            );
        }
        placement.display = display;
    });
}

pub fn restore(window: &slint::Window, placement: &WindowPlacement) {
    let target = window
        .with_winit_window(|native| {
            let monitor = native
                .available_monitors()
                .find(|monitor| display_key(monitor) == placement.display)
                .or_else(|| native.current_monitor())?;
            let saved = placement.displays.get(&display_key(&monitor))?;
            let origin = monitor.position();
            let scale = monitor.scale_factor();
            let screen = monitor.size();
            let outer = native.outer_size();
            let inner = native.inner_size();
            let borders = [
                outer.width.saturating_sub(inner.width),
                outer.height.saturating_sub(inner.height),
            ];
            let (x, width) = constrain_axis(
                origin.x,
                screen.width,
                borders[0],
                saved.offset[0],
                saved.size[0],
                scale,
            );
            let (y, height) = constrain_axis(
                origin.y,
                screen.height,
                borders[1],
                saved.offset[1],
                saved.size[1],
                scale,
            );
            Some((
                slint::PhysicalPosition::new(x, y),
                slint::PhysicalSize::new(width, height),
                saved.maximized,
            ))
        })
        .flatten();
    if let Some((position, size, maximized)) = target {
        window.set_size(size);
        window.set_position(position);
        window.set_maximized(maximized);
    }
}

fn constrain_axis(
    origin: i32,
    screen: u32,
    border: u32,
    offset: i32,
    size: u32,
    scale: f64,
) -> (i32, u32) {
    let maximum = screen.saturating_sub(border).max(1);
    let size = ((f64::from(size) * scale).round() as u32).clamp(1, maximum);
    let offset = ((f64::from(offset) * scale).round() as u32).min(maximum - size);
    (
        origin.saturating_add(offset.min(i32::MAX as u32) as i32),
        size,
    )
}

#[cfg(test)]
#[test]
fn placement_scales_and_remains_inside_the_current_monitor() {
    assert_eq!(
        constrain_axis(-1920, 1920, 16, 100, 900, 1.5),
        (-1770, 1350)
    );
    assert_eq!(constrain_axis(0, 1024, 16, 900, 1200, 2.0), (0, 1008));
    assert_eq!(constrain_axis(100, 1000, 16, -50, 400, 1.0), (100, 400));
}
