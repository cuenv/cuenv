//! Tests for configuration limits

use cuenv_core::Limits;

#[test]
fn test_limits_default() {
    let limits = Limits::default();
    
    assert_eq!(limits.max_path_length, 4096);
    assert_eq!(limits.max_package_name_length, 256);
    assert_eq!(limits.max_output_size, 100 * 1024 * 1024); // 100MB
}

#[test]
fn test_limits_custom() {
    let limits = Limits {
        max_path_length: 1000,
        max_package_name_length: 50,
        max_output_size: 10 * 1024 * 1024, // 10MB
    };
    
    assert_eq!(limits.max_path_length, 1000);
    assert_eq!(limits.max_package_name_length, 50);
    assert_eq!(limits.max_output_size, 10 * 1024 * 1024);
}

#[test]
fn test_limits_modification() {
    let mut limits = Limits {
        max_path_length: 2048,
        max_package_name_length: 128,
        max_output_size: 50 * 1024 * 1024,
    };
    
    assert_eq!(limits.max_path_length, 2048);
    assert_eq!(limits.max_package_name_length, 128);
    assert_eq!(limits.max_output_size, 50 * 1024 * 1024);
    
    // Test modification after creation
    limits.max_path_length = 4096;
    assert_eq!(limits.max_path_length, 4096);
}

#[test]
fn test_limits_edge_values() {
    let limits = Limits {
        max_path_length: 0,
        max_package_name_length: 0,
        max_output_size: 0,
    };
    
    assert_eq!(limits.max_path_length, 0);
    assert_eq!(limits.max_package_name_length, 0);
    assert_eq!(limits.max_output_size, 0);
    
    let limits = Limits {
        max_path_length: usize::MAX,
        max_package_name_length: usize::MAX,
        max_output_size: usize::MAX,
    };
    
    assert_eq!(limits.max_path_length, usize::MAX);
    assert_eq!(limits.max_package_name_length, usize::MAX);
    assert_eq!(limits.max_output_size, usize::MAX);
}