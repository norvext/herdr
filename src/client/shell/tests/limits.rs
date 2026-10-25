use super::*;

fn usage(weekly_only: bool) -> crate::protocol::endpoint::EndpointUsage {
    let mut windows = vec![crate::usage::UsageWindow {
        used_percent: 22,
        duration_minutes: 10080,
        resets_at: 1900000000,
    }];
    if !weekly_only {
        windows.insert(
            0,
            crate::usage::UsageWindow {
                used_percent: 41,
                duration_minutes: 300,
                resets_at: 1900000000,
            },
        );
    }
    crate::protocol::endpoint::EndpointUsage {
        boot_id: "boot-1".into(),
        usage: Some(crate::usage::CodexUsage {
            plan_type: Some("pro".into()),
            windows,
            updated_at: 1800000000,
            stale: false,
        }),
    }
}

fn text(state: &mut ClientShellState) -> String {
    state
        .compose(100, 30)
        .unwrap()
        .cells
        .iter()
        .map(|c| c.symbol.as_str())
        .collect()
}

#[test]
fn limits_render_actual_windows_and_keep_section_drag_geometry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let legacy = text(&mut state);
    assert!(!legacy.contains("Usage Limits"));
    assert_eq!(state.hits.sidebar_sections_area.height, 30);
    state.set_endpoint_usage(&ClientEndpointId::Local, 7, usage(true));
    let frame = text(&mut state);
    assert!(frame.contains("Codex · pro"));
    assert!(frame.contains("Usage Limits"));
    assert!(!frame.contains("UTC"));
    assert!(frame.contains("7d  22%"));
    assert!(!frame.contains("5h"));
    assert_eq!(state.hits.sidebar_sections_area.height, 24);
    assert_eq!(state.hits.sidebar_divider.height, 30);
    assert_eq!(state.hits.sidebar_toggle.y, 29);
    let divider = state.hits.sidebar_section_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: divider.x + 2,
        row: divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: divider.x + 2,
        row: 15,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!((state.sidebar_section_split - 0.625).abs() < 0.001);
    state.set_endpoint_usage(&ClientEndpointId::Local, 7, usage(false));
    assert!(text(&mut state).contains("5h  41%"));
}

#[test]
fn limits_reject_old_generations_boots_and_clear_on_disconnect() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 7, Box::new(snapshot()));
    state.set_endpoint_usage(&ClientEndpointId::Local, 6, usage(true));
    assert!(state.endpoints[0].codex_usage.is_none());
    let mut wrong_boot = usage(true);
    wrong_boot.boot_id = "old-boot".into();
    state.set_endpoint_usage(&ClientEndpointId::Local, 7, wrong_boot);
    assert!(state.endpoints[0].codex_usage.is_none());
    state.set_endpoint_usage(&ClientEndpointId::Local, 7, usage(true));
    assert!(state.endpoints[0].codex_usage.is_some());
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert!(state.endpoints[0].codex_usage.is_none());
}

#[test]
fn limits_follow_selected_endpoint_and_expired_windows_do_not_imply_a_reset() {
    use crate::client::endpoint::{ProfileId, SavedSshEndpoint};
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_usage(&ClientEndpointId::Local, 7, usage(true));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Build".into(),
        target: "dev@build.example".into(),
        session: "agents".into(),
        enabled: true,
    };
    let remote = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_snapshot_for_generation(&remote, 8, Box::new(snapshot()));
    state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
    assert!(text(&mut state).contains("Codex · pro"));
    assert!(state.activate_endpoint_projection(&remote));
    assert!(!text(&mut state).contains("Codex · pro"));
    let mut expired = usage(false);
    let data = expired.usage.as_mut().unwrap();
    data.plan_type = Some("plus".into());
    data.updated_at = 1;
    for window in &mut data.windows {
        window.resets_at = 2;
    }
    state.set_endpoint_usage(&remote, 8, expired);
    let frame = text(&mut state);
    assert!(frame.contains("Codex · plus"));
    let reset = crate::platform::local_datetime_at(2).unwrap();
    assert!(frame.contains(&format!("{:02}:{:02}", reset.hour(), reset.minute())));
    assert!(frame.contains("41%"));
    assert!(!frame.contains("Codex · pro"));
}
