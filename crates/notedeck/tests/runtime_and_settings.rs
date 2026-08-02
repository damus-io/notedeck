use notedeck::{
    RuntimeThreadBudget, Settings, WebsocketConnectionLimit, DEFAULT_WEBSOCKET_CONNECTION_LIMIT,
    MIN_CUSTOM_WEBSOCKET_CONNECTION_LIMIT,
};

#[test]
fn runtime_budget_reserves_ui_outbox_and_sync_jobs() {
    let budget = RuntimeThreadBudget::from_core_count(1);
    assert_eq!(budget.main_async_threads(), 1);
    assert_eq!(budget.sync_job_threads(), 1);

    let budget = RuntimeThreadBudget::from_core_count(4);
    assert_eq!(budget.main_async_threads(), 1);
    assert_eq!(budget.sync_job_threads(), 1);

    let budget = RuntimeThreadBudget::from_core_count(8);
    assert_eq!(budget.main_async_threads(), 4);
    assert_eq!(budget.sync_job_threads(), 2);
}

#[test]
fn settings_defaults_preserve_existing_outbox_behavior() {
    let mut value = serde_json::to_value(Settings::default()).expect("settings json");
    let object = value.as_object_mut().expect("settings object");
    object.remove("columns_use_outbox_relays");
    object.remove("websocket_connection_limit");

    let settings: Settings = serde_json::from_value(value).expect("deserialize settings");

    assert!(settings.columns_use_outbox_relays);
    assert_eq!(
        settings.websocket_connection_limit,
        WebsocketConnectionLimit::Custom(DEFAULT_WEBSOCKET_CONNECTION_LIMIT)
    );
}

#[test]
fn auto_websocket_connection_limit_remains_unbounded_extreme_mode() {
    assert_eq!(WebsocketConnectionLimit::Auto.max_connections(), None);
}

#[test]
fn custom_websocket_connection_limit_is_clamped_to_minimum() {
    assert_eq!(MIN_CUSTOM_WEBSOCKET_CONNECTION_LIMIT, 3);
    assert_eq!(
        WebsocketConnectionLimit::custom(1).max_connections(),
        Some(usize::from(MIN_CUSTOM_WEBSOCKET_CONNECTION_LIMIT))
    );
    assert_eq!(
        WebsocketConnectionLimit::custom(MIN_CUSTOM_WEBSOCKET_CONNECTION_LIMIT).max_connections(),
        Some(usize::from(MIN_CUSTOM_WEBSOCKET_CONNECTION_LIMIT))
    );
}

#[test]
fn websocket_connection_limit_deserializes_to_supported_minimum() {
    let limit: WebsocketConnectionLimit =
        serde_json::from_str(r#"{"Custom":1}"#).expect("deserialize websocket limit");

    assert_eq!(
        limit,
        WebsocketConnectionLimit::Custom(MIN_CUSTOM_WEBSOCKET_CONNECTION_LIMIT)
    );
}
