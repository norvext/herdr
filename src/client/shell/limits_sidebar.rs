use super::render::{put_text, ShellRenderState};
use super::*;

fn usage_color(percent: u8, muted: bool, palette: &Palette) -> ratatui::style::Color {
    if muted {
        palette.overlay0
    } else if percent >= 85 {
        ratatui::style::Color::Rgb(0xe0, 0x6c, 0x6c)
    } else if percent >= 60 {
        ratatui::style::Color::Rgb(0xe0, 0xb4, 0x5a)
    } else {
        palette.teal
    }
}

fn active_usage<'a>(
    endpoints: &'a [ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
) -> Option<&'a crate::usage::CodexUsage> {
    let endpoint = endpoints
        .iter()
        .find(|e| &e.endpoint_id == active_endpoint_id)?;
    if endpoint.status != ClientEndpointStatus::Online {
        return None;
    }
    let (generation, projection) = endpoint.codex_usage.as_ref()?;
    if endpoint
        .snapshot_generation
        .is_some_and(|g| g != *generation)
        || endpoint.snapshot.as_ref()?.boot_id != projection.boot_id
    {
        return None;
    }
    projection.usage.as_ref()
}

fn sections(area: Rect, windows: usize) -> (Rect, Rect) {
    let height = u16::try_from(windows)
        .unwrap_or(u16::MAX)
        .saturating_mul(row_height(area.width))
        .saturating_add(4);
    if windows == 0 || area.width < 18 || area.height < height.saturating_add(8) {
        return (area, Rect::default());
    }
    (
        Rect::new(area.x, area.y, area.width, area.height - height),
        Rect::new(
            area.x,
            area.bottom() - height,
            area.width.saturating_sub(1),
            height,
        ),
    )
}

fn window_label(minutes: u32) -> String {
    match minutes {
        300 => "5h".into(),
        10080 => "7d".into(),
        n if n.is_multiple_of(1440) => format!("{}d", n / 1440),
        n if n.is_multiple_of(60) => format!("{}h", n / 60),
        n => format!("{n}m"),
    }
}

fn row_height(sidebar_width: u16) -> u16 {
    if sidebar_width < 27 {
        2
    } else {
        1
    }
}

fn reset_day(reset: time::PrimitiveDateTime, today: time::Date) -> String {
    if reset.date() == today {
        "сегодня".into()
    } else {
        let weekday = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"]
            [reset.weekday().number_days_from_monday() as usize];
        format!(
            "{weekday} {:02}.{:02}",
            reset.day(),
            u8::from(reset.month())
        )
    }
}

pub(super) fn reset_labels(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    now: i64,
) -> Vec<(String, String)> {
    let Some(usage) = active_usage(endpoints, active_endpoint_id) else {
        return Vec::new();
    };
    let today = crate::platform::local_datetime_at(now).map(|date| date.date());
    usage
        .windows
        .iter()
        .map(
            |window| match (crate::platform::local_datetime_at(window.resets_at), today) {
                (Some(reset), Some(today)) => (
                    reset_day(reset, today),
                    format!("{:02}:{:02}", reset.hour(), reset.minute()),
                ),
                _ => (String::new(), "—".into()),
            },
        )
        .collect()
}

fn centered_text(buffer: &mut Buffer, area: Rect, text: &str, style: Style) {
    let width = UnicodeWidthStr::width(text).min(usize::from(area.width)) as u16;
    put_text(
        buffer,
        area.x + area.width.saturating_sub(width) / 2,
        area.y,
        width,
        text,
        style,
    );
}

fn render_window(
    buffer: &mut Buffer,
    area: Rect,
    window: &crate::usage::UsageWindow,
    labels: (&str, &str),
    color: ratatui::style::Color,
    palette: &Palette,
) {
    let (day, clock) = labels;
    let prefix = format!(
        "{} {:>3}%",
        window_label(window.duration_minutes),
        window.used_percent
    );
    let prefix_width = UnicodeWidthStr::width(prefix.as_str()).min(usize::from(area.width)) as u16;
    let bar_width = area.width.saturating_sub(prefix_width + 2);
    let y = area.bottom().saturating_sub(1);
    let bar = Rect::new(area.x + prefix_width + 1, y, bar_width, 1);
    let inner = Rect::new(bar.x + 1, y, bar.width.saturating_sub(2), 1);
    let style = Style::default();
    buffer.set_style(
        inner,
        Style::default().fg(palette.text).bg(palette.surface0),
    );
    let filled = Rect::new(
        inner.x,
        y,
        (u32::from(inner.width) * u32::from(window.used_percent) / 100) as u16,
        1,
    );
    buffer.set_style(filled, Style::default().fg(palette.panel_bg).bg(color));
    put_text(buffer, bar.x, y, 1, "▏", Style::default().fg(color));
    if bar.width > 1 {
        put_text(
            buffer,
            bar.right() - 1,
            y,
            1,
            "▕",
            Style::default().fg(color),
        );
    }
    if area.height > 1 {
        centered_text(
            buffer,
            Rect::new(area.x, area.y, area.width, 1),
            day,
            Style::default().fg(palette.overlay0),
        );
        centered_text(buffer, inner, clock, style);
    } else {
        let label = format!("{day} {clock}");
        centered_text(buffer, inner, &label, style);
    }
    put_text(
        buffer,
        area.x,
        y,
        prefix_width,
        &prefix,
        Style::default().fg(color),
    );
}

pub(super) fn render(
    buffer: &mut Buffer,
    area: Rect,
    config: &ClientShellConfig,
    state: &ShellRenderState<'_>,
) -> Rect {
    let Some(usage) = active_usage(state.endpoints, state.active_endpoint_id) else {
        return area;
    };
    let (main, panel) = sections(area, usage.windows.len());
    if panel.is_empty() {
        return area;
    }
    let palette = &config.palette;
    put_text(
        buffer,
        panel.x,
        panel.y,
        panel.width,
        &"─".repeat(panel.width as usize),
        Style::default().fg(palette.surface_dim),
    );
    put_text(
        buffer,
        panel.x,
        panel.y + 1,
        panel.width,
        if usage.stale {
            " Usage Limits · stale"
        } else {
            " Usage Limits"
        },
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let name = usage
        .plan_type
        .as_deref()
        .map_or_else(|| " Codex".to_owned(), |plan| format!(" Codex · {plan}"));
    put_text(
        buffer,
        panel.x,
        panel.y + 2,
        panel.width,
        &name,
        Style::default().fg(palette.text),
    );
    for (i, window) in usage.windows.iter().enumerate() {
        let height = row_height(area.width);
        let y = panel.y + 3 + i as u16 * height;
        let expired = state.now >= window.resets_at;
        let color = usage_color(window.used_percent, usage.stale || expired, palette);
        let (day, clock) = state
            .usage_reset_labels
            .get(i)
            .map(|(day, clock)| (day.as_str(), clock.as_str()))
            .unwrap_or(("", "—"));
        render_window(
            buffer,
            Rect::new(panel.x + 1, y, panel.width.saturating_sub(1), height),
            window,
            (day, clock),
            color,
            palette,
        );
    }
    main
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_colors_follow_consumption_thresholds() {
        use ratatui::style::Color;

        let palette = Palette::catppuccin();
        for (percent, expected) in [
            (0, palette.teal),
            (59, palette.teal),
            (60, Color::Rgb(0xe0, 0xb4, 0x5a)),
            (84, Color::Rgb(0xe0, 0xb4, 0x5a)),
            (85, Color::Rgb(0xe0, 0x6c, 0x6c)),
            (100, Color::Rgb(0xe0, 0x6c, 0x6c)),
        ] {
            assert_eq!(usage_color(percent, false, &palette), expected);
            assert_eq!(usage_color(percent, true, &palette), palette.overlay0);
        }
    }

    #[test]
    fn reset_dates_use_today_and_russian_weekdays_across_year_boundaries() {
        let today = time::Date::from_calendar_date(2026, time::Month::October, 25).unwrap();
        assert_eq!(
            reset_day(today.with_hms(23, 59, 0).unwrap(), today),
            "сегодня"
        );
        let monday = today.next_day().unwrap();
        assert_eq!(
            reset_day(monday.with_hms(0, 1, 0).unwrap(), today),
            "пн 26.10"
        );
        let next_year = time::Date::from_calendar_date(2027, time::Month::January, 1).unwrap();
        assert_eq!(
            reset_day(next_year.midnight(), next_year.previous_day().unwrap()),
            "пт 01.01"
        );
    }

    #[test]
    fn bars_keep_usage_first_centered_labels_and_right_padding() {
        let palette = Palette::catppuccin();
        for width in [16, 24, 34, 60] {
            for percent in [0, 50, 100] {
                let height = row_height(width + 2);
                let area = Rect::new(2, 1, width, height);
                let mut buffer = Buffer::empty(Rect::new(0, 0, width + 4, height + 2));
                let window = crate::usage::UsageWindow {
                    used_percent: percent,
                    duration_minutes: 300,
                    resets_at: 1900000000,
                };
                render_window(
                    &mut buffer,
                    area,
                    &window,
                    ("пн 26.10", "14:30"),
                    palette.teal,
                    &palette,
                );
                let y = area.bottom() - 1;
                let prefix = format!("5h {percent:>3}%");
                let bar_x = area.x + prefix.len() as u16 + 1;
                let bar_right = area.right() - 1;
                assert_eq!(buffer[(bar_x, y)].symbol(), "▏");
                assert_eq!(buffer[(bar_right - 1, y)].symbol(), "▕");
                let inner_width = bar_right - bar_x - 2;
                let filled_width = u32::from(inner_width) * u32::from(percent) / 100;
                for index in 0..inner_width {
                    let cell = &buffer[(bar_x + 1 + index, y)];
                    assert_eq!(
                        cell.bg,
                        if u32::from(index) < filled_width {
                            palette.teal
                        } else {
                            palette.surface0
                        }
                    );
                }
                let actual_prefix: String = (area.x..bar_x - 1)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                assert_eq!(actual_prefix, prefix);
                let label = if height == 1 {
                    "пн 26.10 14:30"
                } else {
                    "14:30"
                };
                let label_width = UnicodeWidthStr::width(label) as u16;
                let label_x = bar_x + 1 + (inner_width - label_width) / 2;
                let actual_label: String = (label_x..label_x + label_width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                assert_eq!(actual_label, label);
                assert_eq!(buffer[(area.x, y)].bg, ratatui::style::Color::Reset);
                assert_eq!(buffer[(area.right() - 1, y)].symbol(), " ");
                assert_eq!(
                    buffer[(area.right() - 1, y)].bg,
                    ratatui::style::Color::Reset
                );
                if height == 2 {
                    let day: String = (area.x..area.right())
                        .map(|x| buffer[(x, area.y)].symbol())
                        .collect();
                    assert!(day.contains("пн 26.10"));
                }
            }
        }
    }

    #[test]
    fn optional_panel_preserves_small_and_legacy_layouts() {
        for height in 0..14 {
            let area = Rect::new(0, 0, 27, height);
            assert_eq!(sections(area, 2), (area, Rect::default()));
        }
        let area = Rect::new(0, 0, 27, 30);
        assert_eq!(sections(area, 0), (area, Rect::default()));
        let (main, panel) = sections(area, 2);
        assert_eq!(main.height, 24);
        assert_eq!(main.bottom(), panel.y);
        assert_eq!(panel.bottom(), area.bottom());
        assert_eq!(sections(area, 1).0.height, 25);
        assert_eq!(window_label(10080), "7d");
        assert_eq!(window_label(300), "5h");
        assert_eq!(window_label(60), "1h");
    }
}
