use super::*;

#[test]
fn pm_dir_of_refuses_the_real_tracker_not_a_temp_one() {
    let fake = PathBuf::from("/tmp/c1210-fakehome");
    let guard = |d| crate::home::guard_tracker_in(d, Some(&fake));
    let env = ProviderEnv::default();
    env.set("CADENCE_PM_DIR", fake.join("pm").to_str().unwrap());
    let err = pm_dir_of_with(&env, guard).unwrap_err().to_string();
    assert!(err.contains("CAD-1210"), "{err}");
    env.set("CADENCE_PM_DIR", fake.join("elsewhere").to_str().unwrap());
    assert!(pm_dir_of_with(&env, guard).is_ok());
}
