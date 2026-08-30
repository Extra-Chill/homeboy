mod browser;
mod daemon;
mod extension_parity;
mod homeboy_path;
mod managed_source;
mod provider;
mod shape;
mod tools;

use super::*;

#[test]
fn scoped_lab_doctor_does_not_replace_complete_capability_observation() {
    assert!(observes_complete_capabilities(RunnerDoctorScope::General));
    assert!(observes_complete_capabilities(RunnerDoctorScope::SecretEnv));
    assert!(!observes_complete_capabilities(
        RunnerDoctorScope::LabOffload
    ));
}
