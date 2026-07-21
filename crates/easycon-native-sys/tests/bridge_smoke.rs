use easycon_native_sys::debug::{DebugHandle, counts};

#[test]
fn safe_handle_owns_and_releases_the_native_resource() {
    let baseline = counts().expect("baseline native counts");
    let handle = DebugHandle::create().expect("create debug handle");

    assert_eq!(
        counts().expect("live counts").live_handles,
        baseline.live_handles + 1
    );
    drop(handle);

    assert_eq!(counts().expect("final counts"), baseline);
}
