#[test]
fn websocket_error_uses_typed_ewebsock_raw_os_error() {
    let source = ewebsock::Error::from(std::io::Error::from_raw_os_error(libc::EMFILE));
    let error = enostr::WebSocketError::from(source);

    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
}
