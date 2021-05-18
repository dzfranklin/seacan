mod module;

// Test test that is a substring of another
#[test]
fn test_in_lib() {}

#[test]
fn test_in_lib_1() {}

#[test]
fn test_in_lib_2() {}

#[cfg(feature = "default_feature")]
#[test]
fn test_default_feature() {}

#[cfg(feature = "non_default_feature")]
#[test]
fn test_non_default_feature() {}
