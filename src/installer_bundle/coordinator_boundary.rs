// Independently authored coordinator guard. Integrate unchanged into
// src/installer_bundle/tests.rs; actual private shipped factories, no argv.
#[test]
fn coordinator_actual_image_bootstrap_and_carrier_custody_factories_remain_closed() {
    assert!(production_image().is_err());
    assert!(production_construction().is_err());
    assert!(production_closed_custody().is_err());
}
