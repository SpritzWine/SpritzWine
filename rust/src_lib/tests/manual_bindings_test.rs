// Integration tests for manual bindings

use wine_api::WINHTTP_CONNECTION_INFO;

#[test]
fn test_winhttp_connection_info_creation() {
    // Test that we can create an instance
    let conn_info = WINHTTP_CONNECTION_INFO::default();
    
    // Test that the size is correct
    assert!(conn_info.cbSize > 0);
    
    // Test that we can access the fields
    let _local_addr = conn_info.LocalAddress;
    let _remote_addr = conn_info.RemoteAddress;
}

#[test]
fn test_winhttp_connection_info_size() {
    let conn_info = WINHTTP_CONNECTION_INFO::default();
    
    // The cbSize should match the actual size of the struct
    let expected_size = std::mem::size_of::<WINHTTP_CONNECTION_INFO>();
    assert_eq!(conn_info.cbSize as usize, expected_size);
}

#[test]
fn test_winhttp_connection_info_layout() {
    // Test that the struct has the expected layout
    let conn_info = WINHTTP_CONNECTION_INFO::default();
    
    // cbSize should be first field (offset 0)
    let cb_size_ptr = &conn_info.cbSize as *const _ as usize;
    let struct_ptr = &conn_info as *const _ as usize;
    assert_eq!(cb_size_ptr, struct_ptr);
}