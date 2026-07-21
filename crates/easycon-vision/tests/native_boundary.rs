#[test]
fn vision_reaches_native_diagnostics_through_a_safe_api() {
    let counts = easycon_vision::native_resource_counts().expect("safe native diagnostics");

    assert_eq!(counts.live_handles, 0);
    assert_eq!(counts.live_allocations, 0);
}
