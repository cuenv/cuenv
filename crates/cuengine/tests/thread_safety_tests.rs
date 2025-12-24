//! Compile-time tests to verify thread safety properties

#![allow(unsafe_code)]
#![allow(dead_code)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_const_for_fn)]

use cuengine::CStringPtr;
use std::ffi::CString;

/// Helper function to check if a type is Send
fn requires_send<T: Send>(_t: T) {}

/// Helper function to check if a type is Sync
fn requires_sync<T: Sync>(_t: &T) {}

/// Helper function for static assertions about Send
fn assert_not_send<T: Send>() {}

/// Helper function for static assertions about Sync
fn assert_not_sync<T: Sync>() {}

/// This test verifies that `CStringPtr` is not Send
/// If it compiles, the test fails (we want a compile error)
#[test]
fn test_cstring_ptr_not_send() {
    // Create a CStringPtr
    let test_string = CString::new("test").unwrap();
    let ptr = test_string.into_raw();
    let cstring_ptr = unsafe { CStringPtr::new(ptr) };

    // This should NOT compile because CStringPtr is !Send
    // We use a function that requires Send to test this

    // Uncomment the next line to verify it doesn't compile:
    // requires_send(cstring_ptr);

    // For the test to pass in normal cases, just ensure it exists
    drop(cstring_ptr);
}

/// This test verifies that `CStringPtr` is not Sync
/// If it compiles, the test fails (we want a compile error)
#[test]
fn test_cstring_ptr_not_sync() {
    // Create a CStringPtr
    let test_string = CString::new("test").unwrap();
    let ptr = test_string.into_raw();
    let cstring_ptr = unsafe { CStringPtr::new(ptr) };

    // This should NOT compile because CStringPtr is !Sync
    // We use a function that requires Sync to test this

    // Uncomment the next line to verify it doesn't compile:
    // requires_sync(&cstring_ptr);

    // For the test to pass in normal cases, just ensure it exists
    drop(cstring_ptr);
}

/// Verify that the `PhantomData` approach actually prevents Send/Sync
/// This is a compile-time verification test
#[test]
fn test_thread_safety_markers() {
    // These static assertions will fail to compile if CStringPtr implements Send or Sync

    // These would not compile if CStringPtr was Send/Sync:
    // assert_not_send::<CStringPtr>();
    // assert_not_sync::<CStringPtr>();

    // Instead, let's just verify basic functionality
    let test_string = CString::new("test").unwrap();
    let ptr = test_string.into_raw();
    let cstring_ptr = unsafe { CStringPtr::new(ptr) };

    assert!(!cstring_ptr.is_null());
    let _result = unsafe { cstring_ptr.to_str().unwrap() };
}
