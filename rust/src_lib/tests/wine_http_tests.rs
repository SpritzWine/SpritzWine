//! Tests for the Wine HTTP module
//!
//! This module contains comprehensive tests for the HTTP functionality
//! provided by the wine_http module.

use wine_api::wine_http::*;

#[test]
fn test_http_methods() {
    assert_eq!(method::GET, "GET");
    assert_eq!(method::POST, "POST");
    assert_eq!(method::PUT, "PUT");
    assert_eq!(method::DELETE, "DELETE");
}

#[test]
fn test_http_status_codes() {
    assert_eq!(status::OK, 200);
    assert_eq!(status::NOT_FOUND, 404);
    assert_eq!(status::INTERNAL_SERVER_ERROR, 500);
}

#[test]
fn test_http_utilities() {
    assert_eq!(util::parse_status_code(200), "OK");
    assert_eq!(util::parse_status_code(404), "Not Found");
    assert_eq!(util::parse_status_code(999), "Unknown Status");
}

#[test]
fn test_http_error_display() {
    let error = HttpError::NotImplemented("Test error".to_string());
    assert_eq!(format!("{}", error), "Not implemented: Test error");
}

#[test]
fn test_create_simple_response() {
    let response = util::create_simple_response(200, "OK");
    assert_eq!(response.s.StatusCode, 200);
    assert_eq!(response.s.ReasonLength, 2); // "OK" has 2 characters
}

#[test]
fn test_create_simple_request() {
    let request = util::create_simple_request("GET", "/test");
    assert_eq!(request.s.UnknownVerbLength, 3); // "GET" has 3 characters
    assert_eq!(request.s.RawUrlLength, 5); // "/test" has 5 characters
}

#[test]
fn test_error_handling() {
    // Test setting and getting last error
    HttpError::set_last_error(1234);
    let error = HttpError::last_error();

    match error {
        HttpError::Win32Error(code) => assert_eq!(code, 1234),
        _ => panic!("Expected Win32Error variant"),
    }

    // Test error display
    assert_eq!(format!("{}", error), "Windows error: 1234");
}

#[test]
fn test_http_server_api_stubs() {
    // Test that stub implementations return appropriate errors
    let result = HttpServerApi::initialize();
    assert!(result.is_err());

    let result = HttpServerApi::terminate();
    assert!(result.is_err());

    let result = HttpServerApi::create_request_queue("test-queue");
    assert!(result.is_err());
}

#[test]
fn test_http_client_api_stubs() {
    // Test that stub implementations return appropriate errors
    let result = HttpClientApi::create_session("Test Agent");
    assert!(result.is_err());
}

#[test]
fn test_http_constants() {
    // Test that HTTP constants are correctly defined
    assert_eq!(server_api::INITIALIZE_SERVER, 1);
    assert_eq!(server_api::INITIALIZE_CONFIG, 2);
    assert_eq!(server_api::CREATE_REQUEST_QUEUE_FLAG_OPEN_EXISTING, 1);
    assert_eq!(server_api::CREATE_REQUEST_QUEUE_FLAG_CONTROLLER, 2);
}

#[test]
fn test_http_headers() {
    // Test that HTTP header constants are correctly defined
    assert_eq!(headers::ACCEPT, "Accept");
    assert_eq!(headers::CONTENT_TYPE, "Content-Type");
    assert_eq!(headers::USER_AGENT, "User-Agent");
    assert_eq!(headers::AUTHORIZATION, "Authorization");
}

#[test]
fn test_http_content_types() {
    // Test that HTTP content type constants are correctly defined
    assert_eq!(content_type::TEXT_PLAIN, "text/plain");
    assert_eq!(content_type::APPLICATION_JSON, "application/json");
    assert_eq!(content_type::TEXT_HTML, "text/html");
    assert_eq!(
        content_type::APPLICATION_OCTET_STREAM,
        "application/octet-stream"
    );
}
